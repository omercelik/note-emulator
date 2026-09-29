//! objdump-compatible formatting, for the differential decoder test.

use crate::decode::{Comp, Insn, Op};

pub const XREG: [&str; 32] = [
    "zero", "ra", "sp", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2", "a3", "a4", "a5",
    "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "t3", "t4", "t5", "t6",
];

fn r(n: u8) -> &'static str { XREG[(n & 31) as usize] }

/// GNU objdump prints a bare CSR name when it knows one, else the number in hex.
fn csr_name(n: u32) -> String {
    let s = match n {
        0x000 => "ustatus", 0x004 => "uie", 0x005 => "utvec", 0x040 => "uscratch", 0x041 => "uepc", 0x042 => "ucause", 0x043 => "utval", 0x044 => "uip",
        0x302 => "medeleg", 0x303 => "mideleg", 0x306 => "mcounteren", 0x320 => "mcountinhibit",
        0x300 => "mstatus", 0x301 => "misa", 0x304 => "mie", 0x305 => "mtvec",
        0x340 => "mscratch", 0x341 => "mepc", 0x342 => "mcause", 0x343 => "mtval", 0x344 => "mip",
        0x7a0 => "tselect", 0x7a1 => "tdata1", 0x7a2 => "tdata2", 0x7a3 => "tdata3", 0x7a5 => "tcontrol",
        0xb00 => "mcycle", 0xb02 => "minstret", 0xb80 => "mcycleh", 0xb82 => "minstreth",
        0xf11 => "mvendorid", 0xf12 => "marchid", 0xf13 => "mimpid", 0xf14 => "mhartid",
        0x3a0..=0x3a3 => return format!("pmpcfg{}", n - 0x3a0),
        0x3b0..=0x3bf => return format!("pmpaddr{}", n - 0x3b0),
        _ => return format!("{:#x}", n),
    };
    s.to_string()
}

/// objdump (and GNU as) print a compressed instruction as the 32-bit instruction it expands to,
/// so that is what `format` does; `format_compressed` gives the literal `c.*` form instead.
pub fn format(i: &Insn) -> String {
    use Op::*;
    // HINT encodings have no 32-bit alias to print, so objdump keeps them in their `c.*` form
    if matches!(i.comp, Comp::Li | Comp::Slli | Comp::Mv | Comp::Add | Comp::Lui) && i.rd == 0 { return format_compressed(i); }
    if i.comp == Comp::Nop && i.imm != 0 { return format_compressed(i); }
    let (rd, rs1, rs2) = (r(i.rd), r(i.rs1), r(i.rs2));
    let m = |s: &str| s.to_string();
    match i.op {
        Illegal => m("unimp"),
        Lui => format!("lui\t{},{:#x}", rd, (i.imm as u32) >> 12),
        Auipc => format!("auipc\t{},{:#x}", rd, (i.imm as u32) >> 12),
        Jal => if i.rd == 0 { format!("j\t{:x}", i.imm as u32) } else if i.rd == 1 { format!("jal\t{:x}", i.imm as u32) } else { format!("jal\t{},{:x}", rd, i.imm as u32) },
        Jalr => match (i.rd, i.imm) {
            (0, 0) if i.rs1 == 1 => m("ret"),
            (0, 0) => format!("jr\t{}", rs1),
            (1, 0) => format!("jalr\t{}", rs1),
            (1, _) => format!("jalr\t{}({})", i.imm, rs1),
            (0, _) => format!("jr\t{}({})", i.imm, rs1),
            _ => format!("jalr\t{},{}({})", rd, i.imm, rs1),
        },
        Beq | Bne | Blt | Bge | Bltu | Bgeu => {
            let n = match i.op { Beq => "beq", Bne => "bne", Blt => "blt", Bge => "bge", Bltu => "bltu", _ => "bgeu" };
            if i.rs2 == 0 {
                let z = match i.op { Beq => Some("beqz"), Bne => Some("bnez"), Blt => Some("bltz"), Bge => Some("bgez"), _ => None };
                if let Some(z) = z { return format!("{}\t{},{:x}", z, rs1, i.imm as u32); }
            }
            if i.rs1 == 0 && matches!(i.op, Blt | Bge) {
                return format!("{}\t{},{:x}", if i.op == Blt { "bgtz" } else { "blez" }, rs2, i.imm as u32);
            }
            format!("{}\t{},{},{:x}", n, rs1, rs2, i.imm as u32)
        }
        Lb | Lh | Lw | Lbu | Lhu => {
            let n = match i.op { Lb => "lb", Lh => "lh", Lw => "lw", Lbu => "lbu", _ => "lhu" };
            format!("{}\t{},{}({})", n, rd, i.imm, rs1)
        }
        Sb | Sh | Sw => {
            let n = match i.op { Sb => "sb", Sh => "sh", _ => "sw" };
            format!("{}\t{},{}({})", n, rs2, i.imm, rs1)
        }
        Addi => match (i.rd, i.rs1, i.imm) {
            (0, 0, 0) => m("nop"),
            (_, 0, _) => format!("li\t{},{}", rd, i.imm),
            (_, _, 0) => format!("mv\t{},{}", rd, rs1),
            _ => format!("addi\t{},{},{}", rd, rs1, i.imm),
        },
        Slti => format!("slti\t{},{},{}", rd, rs1, i.imm),
        Sltiu => if i.imm == 1 { format!("seqz\t{},{}", rd, rs1) } else { format!("sltiu\t{},{},{}", rd, rs1, i.imm) },
        Xori => if i.imm == -1 { format!("not\t{},{}", rd, rs1) } else { format!("xori\t{},{},{}", rd, rs1, i.imm) },
        Ori => format!("ori\t{},{},{}", rd, rs1, i.imm),
        Andi => if i.imm == 255 { format!("zext.b\t{},{}", rd, rs1) } else { format!("andi\t{},{},{}", rd, rs1, i.imm) },
        Slli => format!("slli\t{},{},{:#x}", rd, rs1, i.imm),
        Srli => format!("srli\t{},{},{:#x}", rd, rs1, i.imm),
        Srai => format!("srai\t{},{},{:#x}", rd, rs1, i.imm),
        Add => if i.rs1 == 0 { format!("mv\t{},{}", rd, rs2) } else { format!("add\t{},{},{}", rd, rs1, rs2) },
        Sub => if i.rs1 == 0 { format!("neg\t{},{}", rd, rs2) } else { format!("sub\t{},{},{}", rd, rs1, rs2) },
        Sltu => if i.rs1 == 0 { format!("snez\t{},{}", rd, rs2) } else { format!("sltu\t{},{},{}", rd, rs1, rs2) },
        Sll => format!("sll\t{},{},{}", rd, rs1, rs2),
        Slt if i.rs1 == 0 => format!("sgtz\t{},{}", rd, rs2),
        Slt if i.rs2 == 0 => format!("sltz\t{},{}", rd, rs1),
        Slt => format!("slt\t{},{},{}", rd, rs1, rs2),
        Xor => format!("xor\t{},{},{}", rd, rs1, rs2),
        Srl => format!("srl\t{},{},{}", rd, rs1, rs2),
        Sra => format!("sra\t{},{},{}", rd, rs1, rs2),
        Or => format!("or\t{},{},{}", rd, rs1, rs2),
        And => format!("and\t{},{},{}", rd, rs1, rs2),
        Mul => format!("mul\t{},{},{}", rd, rs1, rs2),
        Mulh => format!("mulh\t{},{},{}", rd, rs1, rs2),
        Mulhsu => format!("mulhsu\t{},{},{}", rd, rs1, rs2),
        Mulhu => format!("mulhu\t{},{},{}", rd, rs1, rs2),
        Div => format!("div\t{},{},{}", rd, rs1, rs2),
        Divu => format!("divu\t{},{},{}", rd, rs1, rs2),
        Rem => format!("rem\t{},{},{}", rd, rs1, rs2),
        Remu => format!("remu\t{},{},{}", rd, rs1, rs2),
        LrW | ScW | AmoSwapW | AmoAddW | AmoXorW | AmoAndW | AmoOrW | AmoMinW | AmoMaxW | AmoMinuW | AmoMaxuW => {
            let n = match i.op { LrW => "lr.w", ScW => "sc.w", AmoSwapW => "amoswap.w", AmoAddW => "amoadd.w", AmoXorW => "amoxor.w", AmoAndW => "amoand.w",
                                 AmoOrW => "amoor.w", AmoMinW => "amomin.w", AmoMaxW => "amomax.w", AmoMinuW => "amominu.w", _ => "amomaxu.w" };
            let sfx = match i.imm & 3 { 3 => ".aqrl", 2 => ".aq", 1 => ".rl", _ => "" };
            if i.op == LrW { format!("{}{}\t{},({})", n, sfx, rd, rs1) } else { format!("{}{}\t{},{},({})", n, sfx, rd, rs2, rs1) }
        }
        Fence => m("fence"),
        FenceI => m("fence.i"),
        Ecall => m("ecall"),
        Ebreak => m("ebreak"),
        Mret => m("mret"),
        Wfi => m("wfi"),
        Csrrw | Csrrs | Csrrc | Csrrwi | Csrrsi | Csrrci => {
            let n = csr_name(i.imm as u32);
            let imm5 = i.rs1;
            match i.op {
                Csrrw if i.rd == 0 => format!("csrw\t{},{}", n, rs1),
                Csrrs if i.rs1 == 0 => format!("csrr\t{},{}", rd, n),
                Csrrs if i.rd == 0 => format!("csrs\t{},{}", n, rs1),
                Csrrc if i.rd == 0 => format!("csrc\t{},{}", n, rs1),
                Csrrwi if i.rd == 0 => format!("csrwi\t{},{}", n, imm5),
                Csrrsi if i.rd == 0 => format!("csrsi\t{},{}", n, imm5),
                Csrrci if i.rd == 0 => format!("csrci\t{},{}", n, imm5),
                Csrrw => format!("csrrw\t{},{},{}", rd, n, rs1),
                Csrrs => format!("csrrs\t{},{},{}", rd, n, rs1),
                Csrrc => format!("csrrc\t{},{},{}", rd, n, rs1),
                Csrrwi => format!("csrrwi\t{},{},{}", rd, n, imm5),
                Csrrsi => format!("csrrsi\t{},{},{}", rd, n, imm5),
                _ => format!("csrrci\t{},{},{}", rd, n, imm5),
            }
        }
    }
}

pub fn format_compressed(i: &Insn) -> String {
    let (rd, rs1, rs2) = (r(i.rd), r(i.rs1), r(i.rs2));
    match i.comp {
        Comp::Addi4spn => format!("c.addi4spn\t{},sp,{}", rd, i.imm),
        Comp::Lw => format!("c.lw\t{},{}({})", rd, i.imm, rs1),
        Comp::Sw => format!("c.sw\t{},{}({})", rs2, i.imm, rs1),
        Comp::Nop => if i.imm != 0 { format!("c.nop\t{}", i.imm) } else { "c.nop".to_string() },
        Comp::Addi => format!("c.addi\t{},{}", rd, i.imm),
        Comp::Jal => format!("c.jal\t{:x}", i.imm as u32),
        Comp::Li => format!("c.li\t{},{}", rd, i.imm),
        Comp::Addi16sp => format!("c.addi16sp\tsp,{}", i.imm),
        Comp::Lui => format!("c.lui\t{},{:#x}", rd, ((i.imm as u32) >> 12) & 0xfffff),
        Comp::Srli => format!("c.srli\t{},{:#x}", rd, i.imm),
        Comp::Srai => format!("c.srai\t{},{:#x}", rd, i.imm),
        Comp::Andi => format!("c.andi\t{},{}", rd, i.imm),
        Comp::Sub => format!("c.sub\t{},{}", rd, rs2),
        Comp::Xor => format!("c.xor\t{},{}", rd, rs2),
        Comp::Or => format!("c.or\t{},{}", rd, rs2),
        Comp::And => format!("c.and\t{},{}", rd, rs2),
        Comp::J => format!("c.j\t{:x}", i.imm as u32),
        Comp::Beqz => format!("c.beqz\t{},{:x}", rs1, i.imm as u32),
        Comp::Bnez => format!("c.bnez\t{},{:x}", rs1, i.imm as u32),
        Comp::Slli => format!("c.slli\t{},{:#x}", rd, i.imm),
        Comp::Lwsp => format!("c.lwsp\t{},{}(sp)", rd, i.imm),
        Comp::Jr => format!("c.jr\t{}", rs1),
        Comp::Mv => format!("c.mv\t{},{}", rd, rs2),
        Comp::Ebreak => "c.ebreak".to_string(),
        Comp::Jalr => format!("c.jalr\t{}", rs1),
        Comp::Add => format!("c.add\t{},{}", rd, rs2),
        Comp::Swsp => format!("c.swsp\t{},{}(sp)", rs2, i.imm),
        Comp::None => format(i),
    }
}
