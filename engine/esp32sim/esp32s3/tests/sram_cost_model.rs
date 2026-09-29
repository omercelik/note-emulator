use emu_core::{Bus, Core};
use esp32s3::{CostClass, Esp32S3SramCostModel, InstructionCost, MmioReadTier, ReceiptId};
use esp_soc::Stop;

const PC: u32 = 0x4038_645b;
const BASE: u32 = 0x3fc8_9000;
const POINTER: u32 = 0x3fc8_9100;
const PROGRAM: &[u8] = include_bytes!("fixtures/tinydraw-sram-kernel.bin");
const RECEIPT: &str = include_str!("fixtures/sram-cost-receipt.json");
const MMIO_RECEIPT: &str = include_str!("fixtures/mmio-read-receipt.json");
const OPCODE_RECEIPT: &str = include_str!("fixtures/opcode-cost-receipt.json");

fn configured_machine() -> (esp32s3::Machine, Esp32S3SramCostModel) {
    let mut machine = esp32s3::machine([0; 6]);
    esp_soc::SocBus::load_bytes(&mut machine.bus, PC, PROGRAM).unwrap();
    machine.bus.write32(BASE + 4, POINTER).unwrap();
    machine.bus.write32(POINTER + 0x1c4, 0x1234_5678).unwrap();
    machine.cores[0].set_pc(PC);
    machine.cores[0].set_ar(2, BASE);
    machine.cores[0].set_ar(3, 7);
    let model = Esp32S3SramCostModel::new();
    machine.set_cost_model(Box::new(model.clone())).unwrap();
    (machine, model)
}

#[test]
fn prices_the_receipted_tinydraw_sram_sequence_deterministically() {
    assert!(RECEIPT.contains("\"straightLineIssueCyclesPerInstruction\": 1"));
    assert!(RECEIPT.contains("\"independentAlignedSramAccessAdditiveCycles\": 0"));
    assert!(RECEIPT.contains("d21db307c7be78ffa0bb82a3e1446219bc0eac22cd557fc633e28fd16cadaa8e"));
    let (mut first_machine, first_model) = configured_machine();
    assert!(matches!(first_machine.run(7), Stop::MaxInsns));
    let first = first_model.ledger();

    assert_eq!(
        first.iter().map(|entry| entry.pc).collect::<Vec<_>>(),
        vec![PC, PC + 2, PC + 4, PC + 7, PC + 10, PC + 13, PC + 16,]
    );
    assert_eq!(first.iter().map(|entry| entry.cycles).sum::<u32>(), 7);
    assert!(first
        .iter()
        .all(|entry| entry.core == 0 && entry.cycles == 1));
    let sram_components = first
        .iter()
        .flat_map(|entry| &entry.components)
        .filter(|component| component.class == CostClass::IndependentSramAccess)
        .collect::<Vec<_>>();
    assert_eq!(sram_components.len(), 2);
    assert!(sram_components.iter().all(|component| {
        component.cycles == 0
            && component.receipt == ReceiptId::Idf61IndependentSramAccess
            && component.receipt.file() == "esp32s3/tests/fixtures/sram-cost-receipt.json"
    }));

    let (mut second_machine, second_model) = configured_machine();
    assert!(matches!(second_machine.run(7), Stop::MaxInsns));
    assert_eq!(second_model.ledger(), first);

    assert!(matches!(first_machine.run(1), Stop::MaxInsns));
    let extended = first_model.ledger();
    assert_eq!(extended.len(), 8);
    assert_eq!(extended[7].pc, PC + 19);
    let CostClass::Instruction(InstructionCost::Branch { taken }) = extended[7].components[0].class
    else {
        panic!("expected the receipted BLTU branch component");
    };
    assert_eq!(extended[7].cycles, if taken { 3 } else { 1 });
}

#[test]
fn refuses_unreceipted_mmio_without_committing_a_ledger_entry() {
    let (mut machine, model) = configured_machine();
    machine.cores[0].set_ar(2, 0x600f_e000);
    let stop = machine.run(1);
    assert!(
        matches!(
            stop,
            Stop::CostModel { ref reason, .. } if reason.contains("register not covered")
        ),
        "{stop:?}"
    );
    assert!(model.ledger().is_empty());
}

#[test]
fn prices_the_receipted_load_use_dependency() {
    const DEPENDENT_PC: u32 = 0x4037_1000;
    const DEPENDENT_PROGRAM: &[u8] = &[0xa8, 0x12, 0xa0, 0x88, 0xc0];
    let mut machine = esp32s3::machine([0; 6]);
    esp_soc::SocBus::load_bytes(&mut machine.bus, DEPENDENT_PC, DEPENDENT_PROGRAM).unwrap();
    machine.bus.write32(BASE + 4, 0x1234_5678).unwrap();
    machine.cores[0].set_pc(DEPENDENT_PC);
    machine.cores[0].set_ar(2, BASE);
    machine.cores[0].set_ar(3, 7);
    let model = Esp32S3SramCostModel::new();
    machine.set_cost_model(Box::new(model.clone())).unwrap();

    assert!(matches!(machine.run(1), Stop::MaxInsns));
    assert!(matches!(machine.run(1), Stop::MaxInsns));
    let ledger = model.ledger();
    assert_eq!(ledger.len(), 2);
    assert_eq!(ledger[1].cycles, 2);
    assert_eq!(
        ledger[1].components[1].class,
        CostClass::Instruction(InstructionCost::LoadUse)
    );
    assert_eq!(
        ledger[1].components[1].receipt,
        ReceiptId::Idf61OpcodeLadders
    );
}

#[test]
fn ignores_special_register_encoding_fields_for_load_use() {
    const HAZARD_PC: u32 = 0x4037_1800;
    // l32i.n a10, a2, 4; rsr.ccount a4
    const PROGRAM: &[u8] = &[0xa8, 0x12, 0x40, 0xea, 0x03];
    let mut machine = esp32s3::machine([0; 6]);
    esp_soc::SocBus::load_bytes(&mut machine.bus, HAZARD_PC, PROGRAM).unwrap();
    machine.bus.write32(BASE + 4, 0x1234_5678).unwrap();
    machine.cores[0].set_pc(HAZARD_PC);
    machine.cores[0].set_ar(2, BASE);
    let model = Esp32S3SramCostModel::new();
    machine.set_cost_model(Box::new(model.clone())).unwrap();

    assert!(matches!(machine.run(1), Stop::MaxInsns));
    assert!(matches!(machine.run(1), Stop::MaxInsns));
    let ledger = model.ledger();
    assert_eq!(ledger[1].cycles, 1);
    assert!(ledger[1]
        .components
        .iter()
        .all(|component| component.class != CostClass::Instruction(InstructionCost::LoadUse)));
}

#[test]
fn ignores_jx_fixed_t_field_for_load_use() {
    const HAZARD_PC: u32 = 0x4037_1900;
    // l32i.n a10, a2, 4; jx a3 (whose fixed t field is 10)
    const PROGRAM: &[u8] = &[0xa8, 0x12, 0xa0, 0x03, 0x00];
    let mut machine = esp32s3::machine([0; 6]);
    esp_soc::SocBus::load_bytes(&mut machine.bus, HAZARD_PC, PROGRAM).unwrap();
    machine.bus.write32(BASE + 4, 0x1234_5678).unwrap();
    machine.cores[0].set_pc(HAZARD_PC);
    machine.cores[0].set_ar(2, BASE);
    machine.cores[0].set_ar(3, HAZARD_PC + 5);
    let model = Esp32S3SramCostModel::new();
    machine.set_cost_model(Box::new(model.clone())).unwrap();

    assert!(matches!(machine.run(1), Stop::MaxInsns));
    assert!(matches!(machine.run(1), Stop::MaxInsns));
    let ledger = model.ledger();
    assert_eq!(ledger[1].cycles, 6);
    assert!(ledger[1]
        .components
        .iter()
        .all(|component| component.class != CostClass::Instruction(InstructionCost::LoadUse)));
}

#[test]
fn prices_taken_and_fallthrough_branches() {
    assert!(
        OPCODE_RECEIPT.contains("db29ec42ccccc958c96153340497592ecc76203166a5a98c696bdd81496c6515")
    );
    const BRANCH_PC: u32 = 0x4037_2000;
    const BNEZ_A8: &[u8] = &[0x56, 0x78, 0xfe];

    for (value, taken, cycles) in [(0, false, 1), (1, true, 3)] {
        let mut machine = esp32s3::machine([0; 6]);
        esp_soc::SocBus::load_bytes(&mut machine.bus, BRANCH_PC, BNEZ_A8).unwrap();
        machine.cores[0].set_pc(BRANCH_PC);
        machine.cores[0].set_ar(8, value);
        let model = Esp32S3SramCostModel::new();
        machine.set_cost_model(Box::new(model.clone())).unwrap();

        assert!(matches!(machine.run(1), Stop::MaxInsns));
        let ledger = model.ledger();
        assert_eq!(ledger[0].cycles, cycles);
        assert_eq!(
            ledger[0].components[0].class,
            CostClass::Instruction(InstructionCost::Branch { taken })
        );
        assert_eq!(
            ledger[0].components[0].receipt,
            ReceiptId::Idf61OpcodeLadders
        );
    }
}

#[test]
fn prices_only_the_exact_receipted_mmio_reads() {
    assert!(
        MMIO_RECEIPT.contains("67d213b0f823452582115e74d18f48f0e4142bfa8851f181388fac83c9245dd6")
    );
    assert!(
        MMIO_RECEIPT.contains("90bee849abeca5449fa8e330497717cf9a01562a106eee0e72b604abf55a50b5")
    );

    for (address, tier, cycles) in [
        (0x600c_1014, MmioReadTier::Fast, 9),
        (0x6000_001c, MmioReadTier::Apb, 15),
        (0x6001_ccd4, MmioReadTier::Nrx, 18),
    ] {
        let (mut machine, model) = configured_machine();
        machine.cores[0].set_ar(2, address - 4);

        assert!(matches!(machine.run(1), Stop::MaxInsns));
        let ledger = model.ledger();
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].cycles, cycles);
        assert_eq!(ledger[0].components.len(), 1);
        assert_eq!(ledger[0].components[0].class, CostClass::MmioRead(tier));
        assert_eq!(
            ledger[0].components[0].receipt,
            ReceiptId::Idf61RegisterBlockRead
        );
        assert_eq!(
            ledger[0].components[0].receipt.file(),
            "esp32s3/tests/fixtures/mmio-read-receipt.json"
        );
    }
}

#[test]
fn refuses_mmio_reads_without_an_exact_receipt() {
    let (mut machine, model) = configured_machine();
    machine.cores[0].set_ar(2, 0x6000_8038 - 4);

    let stop = machine.run(1);
    assert!(
        matches!(stop, Stop::CostModel { ref reason, .. } if reason.contains("distribution")),
        "{stop:?}"
    );
    assert!(model.ledger().is_empty());
}

#[test]
fn does_not_apply_the_sram_load_use_cost_after_mmio() {
    let (mut machine, model) = configured_machine();
    machine.cores[0].set_ar(2, 0x6000_001c - 4);

    assert!(matches!(machine.run(2), Stop::MaxInsns));
    let ledger = model.ledger();
    assert_eq!(ledger.len(), 2);
    assert_eq!(ledger[0].cycles, 15);
    assert_eq!(ledger[1].cycles, 1);
    assert!(ledger[1]
        .components
        .iter()
        .all(|component| component.class != CostClass::Instruction(InstructionCost::LoadUse)));
}
