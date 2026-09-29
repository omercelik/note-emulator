use emu_core::{Bus, Core};
use esp32sim_wasm_jit::{compile_shared_sram_block, compile_sram_block, REGISTER_COUNT};
use esp_soc::SocBus;
use std::process::Command;

const KERNEL: &[u8] = include_bytes!("../../esp32s3/tests/fixtures/tinydraw-sram-kernel.bin");
const KERNEL_START: u32 = 0x4038_645b;
const KERNEL_INSTRUCTIONS: u64 = 7;
const KERNEL_BYTES: usize = 19;
const SRAM_BASE: u32 = 0x3fc8_9000;
const SRAM_LEN: usize = 0x400;

#[test]
fn wasm_kernel_matches_the_receipt_priced_interpreter() {
    let mut initial_registers = [0; REGISTER_COUNT];
    initial_registers[2] = SRAM_BASE;
    initial_registers[3] = 7;
    let mut initial_sram = vec![0; SRAM_LEN];
    initial_sram[4..8].copy_from_slice(&0x3fc8_9100u32.to_le_bytes());
    initial_sram[0x2c4..0x2c8].copy_from_slice(&0x1234_5678u32.to_le_bytes());

    let compiled = compile_sram_block(
        KERNEL_START,
        &KERNEL[..KERNEL_BYTES],
        initial_registers,
        SRAM_BASE,
        &initial_sram,
    )
    .expect("the receipted SRAM kernel compiles");
    let actual = execute_node(&compiled.bytes);
    let expected = interpret(&initial_sram);

    assert_eq!(compiled.instruction_count, KERNEL_INSTRUCTIONS as usize);
    assert_eq!(compiled.cycle_cost, expected.cycles);
    assert_eq!(actual, expected);
}

#[test]
fn shared_memory_kernel_updates_the_browser_handoff_record() {
    let compiled = compile_shared_sram_block(
        KERNEL_START,
        &KERNEL[..KERNEL_BYTES],
        128,
        8192,
        SRAM_BASE,
        SRAM_LEN,
    )
    .expect("the shared-memory kernel compiles");
    const SCRIPT: &str = r#"
const fs = require('fs');
const memory = new WebAssembly.Memory({initial: 1});
const view = new DataView(memory.buffer);
view.setUint32(128 + 2 * 4, 0x3fc89000, true);
view.setUint32(128 + 3 * 4, 7, true);
view.setUint32(128 + 64, 0x4038645b, true);
view.setUint32(8192 + 4, 0x3fc89100, true);
view.setUint32(8192 + 0x2c4, 0x12345678, true);
WebAssembly.instantiate(fs.readFileSync(process.argv[1]), {env: {memory}}).then(({instance}) => {
  instance.exports.run();
  process.stdout.write(`${view.getUint32(128 + 11 * 4, true)},${view.getUint32(128 + 64, true)},${view.getBigUint64(128 + 72, true)}`);
}).catch(error => { console.error(error); process.exit(1); });
"#;
    let path = write_module(&compiled.bytes, "shared");
    let output = Command::new("node")
        .args(["-e", SCRIPT])
        .arg(&path)
        .output()
        .expect("run shared-memory module under Node");
    std::fs::remove_file(path).expect("remove wasm module");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "1,1077437550,7");
}

#[test]
fn refuses_an_instruction_outside_the_receipted_slice() {
    let error = compile_sram_block(
        KERNEL_START,
        &[0x22, 0xa0, 0x00],
        [0; REGISTER_COUNT],
        SRAM_BASE,
        &[0; 4],
    )
    .expect_err("movi is outside this first slice");
    assert!(error.to_string().contains("cost not adopted"), "{error}");
}

#[test]
fn refuses_a_non_sram_data_image() {
    let error = compile_sram_block(
        KERNEL_START,
        &KERNEL[..KERNEL_BYTES],
        [0; REGISTER_COUNT],
        0x6000_0000,
        &[0; 4],
    )
    .expect_err("MMIO must not be priced as SRAM");
    assert!(
        error.to_string().contains("outside internal DRAM"),
        "{error}"
    );
}

#[test]
fn traps_a_runtime_load_outside_the_supplied_sram_image() {
    let mut registers = [0; REGISTER_COUNT];
    registers[2] = SRAM_BASE + SRAM_LEN as u32;
    let compiled = compile_sram_block(KERNEL_START, &KERNEL[..2], registers, SRAM_BASE, &[0; 4])
        .expect("the load encoding and SRAM class are valid");
    assert!(emitted_module_traps(&compiled.bytes, "bounds"), "out-of-image load did not trap");
}

#[test]
fn traps_a_runtime_unaligned_load_that_the_cost_model_refuses() {
    let mut registers = [0; REGISTER_COUNT];
    registers[2] = SRAM_BASE + 1;
    let compiled = compile_sram_block(KERNEL_START, &KERNEL[..2], registers, SRAM_BASE, &[0; 16])
        .expect("the load encoding is valid and guarded at runtime");
    assert!(emitted_module_traps(&compiled.bytes, "alignment"), "unaligned SRAM load did not trap");
}

#[derive(Debug, Eq, PartialEq)]
struct ResultState {
    registers: [u32; REGISTER_COUNT],
    pc: u32,
    cycles: u64,
    sram: Vec<u8>,
}

fn interpret(initial_sram: &[u8]) -> ResultState {
    let mut machine = esp32s3::machine([0; 6]);
    SocBus::load_bytes(&mut machine.bus, KERNEL_START, KERNEL).unwrap();
    SocBus::load_bytes(&mut machine.bus, SRAM_BASE, initial_sram).unwrap();
    machine.cores[0].set_pc(KERNEL_START);
    machine.cores[0].set_ar(2, SRAM_BASE);
    machine.cores[0].set_ar(3, 7);
    let model = esp32s3::Esp32S3SramCostModel::new();
    machine.set_cost_model(Box::new(model.clone())).unwrap();

    assert!(matches!(
        machine.run(KERNEL_INSTRUCTIONS),
        esp32s3::Stop::MaxInsns
    ));
    ResultState {
        registers: std::array::from_fn(|index| machine.cores[0].get_ar(index as u8)),
        pc: machine.cores[0].pc(),
        cycles: model
            .ledger()
            .iter()
            .map(|entry| u64::from(entry.cycles))
            .sum(),
        sram: (0..SRAM_LEN)
            .map(|offset| machine.bus.read8(SRAM_BASE + offset as u32).unwrap())
            .collect(),
    }
}

fn execute_node(module: &[u8]) -> ResultState {
    const SCRIPT: &str = r#"
const fs = require('fs');
WebAssembly.instantiate(fs.readFileSync(process.argv[1])).then(({instance}) => {
  instance.exports.run();
  const memory = instance.exports.memory.buffer;
  const view = new DataView(memory);
  const registers = [];
  for (let index = 0; index < 16; index++) registers.push(view.getUint32(index * 4, true));
  const sram = Buffer.from(new Uint8Array(memory, 4096, 1024)).toString('hex');
  process.stdout.write(`${registers.join(',')}\n${view.getUint32(64, true)}\n${view.getBigUint64(72, true)}\n${sram}`);
}).catch(error => { console.error(error); process.exit(1); });
"#;
    let path = write_module(module, "kernel");
    let output = Command::new("node")
        .args(["-e", SCRIPT])
        .arg(&path)
        .output()
        .expect("run emitted module under Node");
    std::fs::remove_file(path).expect("remove wasm module");
    assert!(
        output.status.success(),
        "Node rejected emitted module: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).expect("Node output is UTF-8");
    let mut lines = text.lines();
    let fields = lines
        .next()
        .expect("register line")
        .split(',')
        .collect::<Vec<_>>();
    assert_eq!(fields.len(), REGISTER_COUNT);
    ResultState {
        registers: std::array::from_fn(|index| fields[index].parse().unwrap()),
        pc: lines.next().expect("pc line").parse().unwrap(),
        cycles: lines.next().expect("cycle line").parse().unwrap(),
        sram: decode_hex(lines.next().expect("SRAM line")),
    }
}

fn emitted_module_traps(module: &[u8], label: &str) -> bool {
    const SCRIPT: &str = r#"
const fs = require('fs');
WebAssembly.instantiate(fs.readFileSync(process.argv[1])).then(({instance}) => {
  instance.exports.run();
  process.exit(2);
}).catch(() => process.exit(0));
"#;
    let path = write_module(module, label);
    let output = Command::new("node")
        .args(["-e", SCRIPT])
        .arg(&path)
        .output()
        .expect("run emitted module under Node");
    std::fs::remove_file(path).expect("remove wasm module");
    output.status.success()
}

fn write_module(module: &[u8], label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "esp32sim-wasm-jit-{label}-{}.wasm",
        std::process::id()
    ));
    std::fs::write(&path, module).expect("write wasm module");
    path
}

fn decode_hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
        .collect()
}
