use esp32sim_wasm_jit::{compile_sram_block, CYCLE_OFFSET, PC_OFFSET, REGISTER_COUNT};
use std::process::Command;

const KERNEL: &[u8] = include_bytes!("../../esp32s3/tests/fixtures/tinydraw-sram-kernel.bin");
const KERNEL_START: u32 = 0x4038_645b;
const KERNEL_BYTES: usize = 19;
const KERNEL_INSTRUCTIONS: u64 = 7;
const SRAM_BASE: u32 = 0x3fc8_9000;
const SRAM_LEN: usize = 0x400;
const WARMUP_RUNS: u64 = 100_000;
const SAMPLE_RUNS: u64 = 1_000_000;
const SAMPLES: usize = 5;

fn main() -> Result<(), String> {
    let mut registers = [0; REGISTER_COUNT];
    registers[2] = SRAM_BASE;
    registers[3] = 7;
    let mut sram = vec![0; SRAM_LEN];
    sram[4..8].copy_from_slice(&0x3fc8_9100u32.to_le_bytes());
    sram[0x2c4..0x2c8].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    let compiled = compile_sram_block(
        KERNEL_START,
        &KERNEL[..KERNEL_BYTES],
        registers,
        SRAM_BASE,
        &sram,
    )
    .map_err(|error| error.to_string())?;
    if compiled.cycle_cost != KERNEL_INSTRUCTIONS {
        return Err(format!(
            "expected {KERNEL_INSTRUCTIONS} receipt-priced cycles, got {}",
            compiled.cycle_cost
        ));
    }

    let module_path = std::env::temp_dir().join(format!(
        "esp32sim-wasm-jit-speed-{}.wasm",
        std::process::id()
    ));
    std::fs::write(&module_path, &compiled.bytes).map_err(|error| error.to_string())?;
    let measurements = run_node(&module_path)?;
    std::fs::remove_file(module_path).map_err(|error| error.to_string())?;

    let expected_cycles = SAMPLE_RUNS * KERNEL_INSTRUCTIONS;
    if measurements.cycles != expected_cycles {
        return Err(format!(
            "runtime reported {} cycles, expected {expected_cycles}",
            measurements.cycles
        ));
    }
    let mut ranked_mips = measurements
        .elapsed_ns
        .iter()
        .map(|elapsed| (SAMPLE_RUNS * KERNEL_INSTRUCTIONS) as f64 * 1_000.0 / *elapsed as f64)
        .collect::<Vec<_>>();
    ranked_mips.sort_by(f64::total_cmp);
    let median_mips = ranked_mips[SAMPLES / 2];

    println!("{{");
    println!("  \"schemaVersion\": 1,");
    println!(
        "  \"implementationHead\": {:?},",
        command("git", &["rev-parse", "HEAD"])?
    );
    println!("  \"hostArchitecture\": {:?},", std::env::consts::ARCH);
    println!("  \"node\": {:?},", command("node", &["--version"])?);
    println!("  \"rustc\": {:?},", command("rustc", &["--version"])?);
    println!("  \"workload\": {{");
    println!("    \"guestInstructionsPerRun\": {KERNEL_INSTRUCTIONS},");
    println!("    \"receiptCyclesPerRun\": {},", compiled.cycle_cost);
    println!("    \"moduleBytes\": {},", compiled.bytes.len());
    println!("    \"warmupRuns\": {WARMUP_RUNS},");
    println!("    \"sampleRuns\": {SAMPLE_RUNS},");
    println!("    \"samples\": {SAMPLES}");
    println!("  }},");
    println!("  \"elapsedNanoseconds\": {:?},", measurements.elapsed_ns);
    println!("  \"rankedMips\": {:?},", ranked_mips);
    println!("  \"medianMips\": {median_mips:.6}");
    println!("}}");
    Ok(())
}

struct Measurements {
    elapsed_ns: Vec<u64>,
    cycles: u64,
}

fn run_node(module_path: &std::path::Path) -> Result<Measurements, String> {
    const SCRIPT: &str = r#"
const fs = require('fs');
const path = process.argv[1];
const pc = Number(process.argv[2]);
const pcOffset = Number(process.argv[3]);
const cycleOffset = Number(process.argv[4]);
const warmup = Number(process.argv[5]);
const runs = Number(process.argv[6]);
const samples = Number(process.argv[7]);
WebAssembly.instantiate(fs.readFileSync(path)).then(({instance}) => {
  const view = new DataView(instance.exports.memory.buffer);
  const run = instance.exports.run;
  for (let i = 0; i < warmup; i++) { view.setUint32(pcOffset, pc, true); run(); }
  const elapsed = [];
  for (let sample = 0; sample < samples; sample++) {
    view.setBigUint64(cycleOffset, 0n, true);
    const started = process.hrtime.bigint();
    for (let i = 0; i < runs; i++) { view.setUint32(pcOffset, pc, true); run(); }
    elapsed.push(Number(process.hrtime.bigint() - started));
  }
  process.stdout.write(`${elapsed.join(',')}\n${view.getBigUint64(cycleOffset, true)}`);
}).catch(error => { console.error(error); process.exit(1); });
"#;
    let output = Command::new("node")
        .args(["-e", SCRIPT])
        .arg(module_path)
        .arg(KERNEL_START.to_string())
        .arg(PC_OFFSET.to_string())
        .arg(CYCLE_OFFSET.to_string())
        .arg(WARMUP_RUNS.to_string())
        .arg(SAMPLE_RUNS.to_string())
        .arg(SAMPLES.to_string())
        .output()
        .map_err(|error| format!("run Node benchmark: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Node benchmark failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    let mut lines = text.lines();
    let elapsed_ns = lines
        .next()
        .ok_or_else(|| "Node benchmark omitted elapsed times".to_string())?
        .split(',')
        .map(|value| value.parse::<u64>().map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    if elapsed_ns.len() != SAMPLES {
        return Err(format!("Node returned {} samples", elapsed_ns.len()));
    }
    let cycles = lines
        .next()
        .ok_or_else(|| "Node benchmark omitted cycle total".to_string())?
        .parse::<u64>()
        .map_err(|error| error.to_string())?;
    Ok(Measurements { elapsed_ns, cycles })
}

fn command(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("run {program}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
