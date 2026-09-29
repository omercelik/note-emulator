//! objdump-compatible disassembly (used by the differential decoder test and the debugger).
use crate::decode::{sr_name, ur_name, Insn, Op};

fn a(n: u8) -> String { format!("a{}", n) }
fn f(n: u8) -> String { format!("f{}", n) }
fn b(n: u8) -> String { format!("b{}", n) }
fn hx(v: i32) -> String { format!("{:x}", v as u32) }

pub fn mnemonic(i: &Insn) -> String {
    use Op::*;
    let base = match i.op {
        Ill => "ill", IllN => "ill.n", Nop => "nop", NopN => "nop.n", Break => "break", BreakN => "break.n", Syscall => "syscall", Simcall => "simcall",
        Waiti => "waiti", Rsil => "rsil", Isync => "isync", Rsync => "rsync", Esync => "esync", Dsync => "dsync", Excw => "excw", Memw => "memw", Extw => "extw",
        J => "j", Jx => "jx", Call0 => "call0", Call4 => "call4", Call8 => "call8", Call12 => "call12", Callx0 => "callx0", Callx4 => "callx4", Callx8 => "callx8", Callx12 => "callx12",
        Ret => "ret", RetN => "ret.n", Retw => "retw", RetwN => "retw.n", Entry => "entry", Movsp => "movsp", Rotw => "rotw",
        Rfe => "rfe", Rfue => "rfue", Rfde => "rfde", Rfwo => "rfwo", Rfwu => "rfwu", Rfi => "rfi", Rfme => "rfme", L32e => "l32e", S32e => "s32e", S32nb => "s32nb",
        Beqz => "beqz", Bnez => "bnez", Bltz => "bltz", Bgez => "bgez", BeqzN => "beqz.n", BnezN => "bnez.n",
        Beqi => "beqi", Bnei => "bnei", Blti => "blti", Bgei => "bgei", Bltui => "bltui", Bgeui => "bgeui",
        Bnone => "bnone", Beq => "beq", Blt => "blt", Bltu => "bltu", Ball => "ball", Bbc => "bbc", Bbci => "bbci", Bany => "bany", Bne => "bne", Bge => "bge", Bgeu => "bgeu", Bnall => "bnall", Bbs => "bbs", Bbsi => "bbsi",
        Bf => "bf", Bt => "bt", Loop => "loop", Loopnez => "loopnez", Loopgtz => "loopgtz",
        L8ui => "l8ui", L16ui => "l16ui", L16si => "l16si", L32i => "l32i", L32iN => "l32i.n", L32r => "l32r", L32ai => "l32ai", S8i => "s8i", S16i => "s16i", S32i => "s32i", S32iN => "s32i.n", S32ri => "s32ri", S32c1i => "s32c1i",
        Dpfr => "dpfr", Dpfw => "dpfw", Dpfro => "dpfro", Dpfwo => "dpfwo", Dhwb => "dhwb", Dhwbi => "dhwbi", Dhi => "dhi", Dii => "dii", Ipf => "ipf", Ihi => "ihi", Iii => "iii", Ipfl => "ipfl", Ihu => "ihu", Iiu => "iiu", Dpfl => "dpfl", Dhu => "dhu", Diu => "diu",
        Movi => "movi", MoviN => "movi.n", Mov => "mov", MovN => "mov.n", Add => "add", AddN => "add.n", Addi => "addi", AddiN => "addi.n", Addmi => "addmi", Sub => "sub", Addx2 => "addx2", Addx4 => "addx4", Addx8 => "addx8", Subx2 => "subx2", Subx4 => "subx4", Subx8 => "subx8",
        And => "and", Or => "or", Xor => "xor", Neg => "neg", Abs => "abs", Extui => "extui", Sext => "sext", Clamps => "clamps", Min => "min", Max => "max", Minu => "minu", Maxu => "maxu",
        Moveqz => "moveqz", Movnez => "movnez", Movltz => "movltz", Movgez => "movgez", Movf => "movf", Movt => "movt",
        Slli => "slli", Srai => "srai", Srli => "srli", Sll => "sll", Srl => "srl", Sra => "sra", Src => "src", Ssr => "ssr", Ssl => "ssl", Ssa8l => "ssa8l", Ssa8b => "ssa8b", Ssai => "ssai", Nsa => "nsa", Nsau => "nsau",
        Mull => "mull", Muluh => "muluh", Mulsh => "mulsh", Mul16u => "mul16u", Mul16s => "mul16s", Quou => "quou", Quos => "quos", Remu => "remu", Rems => "rems", Salt => "salt", Saltu => "saltu",
        Rsr => "rsr", Wsr => "wsr", Xsr => "xsr", Rur => "rur", Wur => "wur", Rer => "rer", Wer => "wer",
        Andb => "andb", Andbc => "andbc", Orb => "orb", Orbc => "orbc", Xorb => "xorb", Any4 => "any4", All4 => "all4", Any8 => "any8", All8 => "all8",
        Ritlb0 => "ritlb0", Iitlb => "iitlb", Pitlb => "pitlb", Witlb => "witlb", Ritlb1 => "ritlb1", Rdtlb0 => "rdtlb0", Idtlb => "idtlb", Pdtlb => "pdtlb", Wdtlb => "wdtlb", Rdtlb1 => "rdtlb1",
        Lsi => "lsi", Ssi => "ssi", Lsip => "lsip", Ssip => "ssip", Lsx => "lsx", Ssx => "ssx", Lsxp => "lsxp", Ssxp => "ssxp",
        AddS => "add.s", SubS => "sub.s", MulS => "mul.s", MaddS => "madd.s", MsubS => "msub.s", MaddnS => "maddn.s", DivnS => "divn.s",
        RoundS => "round.s", TruncS => "trunc.s", FloorS => "floor.s", CeilS => "ceil.s", FloatS => "float.s", UfloatS => "ufloat.s", UtruncS => "utrunc.s",
        MovS => "mov.s", AbsS => "abs.s", NegS => "neg.s", Rfr => "rfr", Wfr => "wfr",
        Div0S => "div0.s", Nexp01S => "nexp01.s", ConstS => "const.s", Recip0S => "recip0.s", Rsqrt0S => "rsqrt0.s", Sqrt0S => "sqrt0.s", MksadjS => "mksadj.s", MkdadjS => "mkdadj.s", AddexpS => "addexp.s", AddexpmS => "addexpm.s",
        UnS => "un.s", OeqS => "oeq.s", UeqS => "ueq.s", OltS => "olt.s", UltS => "ult.s", OleS => "ole.s", UleS => "ule.s",
        MoveqzS => "moveqz.s", MovnezS => "movnez.s", MovltzS => "movltz.s", MovgezS => "movgez.s", MovfS => "movf.s", MovtS => "movt.s",
        Mac16 => return mac16_mnemonic(i),
        Pie => return crate::pie::format(i.raw, i.imm as usize),
    };
    match i.op {
        Rsr | Wsr | Xsr => match sr_name(i.imm as u32) { Some(n) => format!("{}.{}", base, n), None => format!("{}.{}", base, i.imm) },
        Rur | Wur => match ur_name(i.imm as u32) { Some(n) => format!("{}.{}", base, n), None => format!("{}.{}", base, i.imm) },
        _ => base.to_string(),
    }
}

fn mac16_mnemonic(i: &Insn) -> String {
    let op1 = (i.raw >> 16) & 0xf;
    let op2 = (i.raw >> 20) & 0xf;
    let half = ["ll", "hl", "lh", "hh"][(op1 & 3) as usize];
    let kind = ["umul", "mul", "mula", "muls"][((op1 >> 2) & 3) as usize];
    match op2 {
        0 => format!("mula.dd.{}.ldinc", half), 1 => format!("mula.dd.{}.lddec", half),
        2 => format!("{}.dd.{}", kind, half), 3 => format!("{}.ad.{}", kind, half),
        4 => format!("mula.da.{}.ldinc", half), 5 => format!("mula.da.{}.lddec", half),
        6 => format!("{}.da.{}", kind, half), 7 => format!("{}.aa.{}", kind, half),
        8 => "ldinc".into(), 9 => "lddec".into(),
        _ => "mac16.?".into(),
    }
}

/// Operands in objdump order/spelling.
pub fn operands(i: &Insn) -> Vec<String> {
    use Op::*;
    let (r, s, t) = (i.r, i.s, i.t);
    match i.op {
        Ill | IllN | Nop | NopN | Syscall | Simcall | Isync | Rsync | Esync | Dsync | Excw | Memw | Extw
        | Ret | RetN | Retw | RetwN | Rfe | Rfue | Rfde | Rfwo | Rfwu | Rfme => vec![],
        Break => vec![i.imm.to_string(), i.imm2.to_string()],
        BreakN => vec![i.imm.to_string()],
        Waiti | Rfi | Ssai => vec![i.imm.to_string()],
        Rotw => vec![i.imm.to_string()],
        Rsil => vec![a(t), i.imm.to_string()],
        J | Call0 | Call4 | Call8 | Call12 => vec![hx(i.imm)],
        Jx | Callx0 | Callx4 | Callx8 | Callx12 => vec![a(s)],
        Entry => vec![a(s), i.imm.to_string()],
        Movsp => vec![a(t), a(s)],
        L32e | S32e | S32nb => vec![a(t), a(s), i.imm.to_string()],
        Beqz | Bnez | Bltz | Bgez | BeqzN | BnezN => vec![a(s), hx(i.imm)],
        Beqi | Bnei | Blti | Bgei | Bltui | Bgeui => vec![a(s), i.imm2.to_string(), hx(i.imm)],
        Bnone | Beq | Blt | Bltu | Ball | Bbc | Bany | Bne | Bge | Bgeu | Bnall | Bbs => vec![a(s), a(t), hx(i.imm)],
        Bbci | Bbsi => vec![a(s), i.imm2.to_string(), hx(i.imm)],
        Bf | Bt => vec![b(s), hx(i.imm)],
        Loop | Loopnez | Loopgtz => vec![a(s), hx(i.imm)],
        L8ui | L16ui | L16si | L32i | L32ai | S8i | S16i | S32i | S32ri | S32c1i | L32iN | S32iN => vec![a(t), a(s), i.imm.to_string()],
        L32r => vec![a(t), hx(i.imm)],
        Dpfr | Dpfw | Dpfro | Dpfwo | Dhwb | Dhwbi | Dhi | Dii | Ipf | Ihi | Iii | Ipfl | Ihu | Iiu | Dpfl | Dhu | Diu => vec![a(s), i.imm.to_string()],
        Movi => vec![a(t), i.imm.to_string()],
        MoviN => vec![a(s), i.imm.to_string()],
        Mov => vec![a(t), a(s)],
        MovN => vec![a(t), a(s)],
        Add | AddN | Sub | Addx2 | Addx4 | Addx8 | Subx2 | Subx4 | Subx8 | And | Or | Xor | Min | Max | Minu | Maxu
        | Moveqz | Movnez | Movltz | Movgez | Src | Mull | Muluh | Mulsh | Mul16u | Mul16s | Quou | Quos | Remu | Rems | Salt | Saltu => vec![a(r), a(s), a(t)],
        Movf | Movt => vec![a(r), a(s), b(t)],
        Addi => vec![a(t), a(s), i.imm.to_string()],
        AddiN => vec![a(r), a(s), i.imm.to_string()],
        Addmi => vec![a(t), a(s), format!("0x{:x}", i.imm as u32)],
        Neg | Abs => vec![a(r), a(t)],
        Extui => vec![a(r), a(t), i.imm.to_string(), i.imm2.to_string()],
        Sext | Clamps => vec![a(r), a(s), i.imm.to_string()],
        Slli => vec![a(r), a(s), i.imm.to_string()],
        Srai | Srli => vec![a(r), a(t), i.imm.to_string()],
        Sll => vec![a(r), a(s)],
        Srl | Sra => vec![a(r), a(t)],
        Ssr | Ssl | Ssa8l | Ssa8b => vec![a(s)],
        Nsa | Nsau => vec![a(t), a(s)],
        Rsr | Wsr | Xsr => vec![a(t)],
        Rur => vec![a(r)],
        Wur => vec![a(t)],
        Rer | Wer => vec![a(t), a(s)],
        Andb | Andbc | Orb | Orbc | Xorb => vec![b(r), b(s), b(t)],
        Any4 | All4 => vec![b(t), (s..s + 4).map(b).collect::<Vec<_>>().join(":")],
        Any8 | All8 => vec![b(t), (s..s + 8).map(b).collect::<Vec<_>>().join(":")],
        Ritlb0 | Pitlb | Ritlb1 | Rdtlb0 | Pdtlb | Rdtlb1 => vec![a(t), a(s)],
        Iitlb | Idtlb => vec![a(s)],
        Witlb | Wdtlb => vec![a(t), a(s)],
        Lsi | Ssi | Lsip | Ssip => vec![f(t), a(s), i.imm.to_string()],
        Lsx | Ssx | Lsxp | Ssxp => vec![f(r), a(s), a(t)],
        AddS | SubS | MulS | MaddS | MsubS | MaddnS | DivnS => vec![f(r), f(s), f(t)],
        RoundS | TruncS | FloorS | CeilS | UtruncS => vec![a(r), f(s), i.imm.to_string()],
        FloatS | UfloatS => vec![f(r), a(s), i.imm.to_string()],
        MovS | AbsS | NegS | Div0S | Nexp01S | Recip0S | Rsqrt0S | Sqrt0S | MksadjS | MkdadjS | AddexpS | AddexpmS => vec![f(r), f(s)],
        ConstS => vec![f(r), i.imm.to_string()],
        Rfr => vec![a(r), f(s)],
        Wfr => vec![f(r), a(s)],
        UnS | OeqS | UeqS | OltS | UltS | OleS | UleS => vec![b(r), f(s), f(t)],
        MoveqzS | MovnezS | MovltzS | MovgezS => vec![f(r), f(s), a(t)],
        MovfS | MovtS => vec![f(r), f(s), b(t)],
        Mac16 => {
            let op2 = (i.raw >> 20) & 0xf;
            match op2 {
                0 | 1 => vec![format!("m{}", r & 3), a(s), format!("m{}", (r >> 2) & 1), format!("m{}", 2 + ((t >> 2) & 1))],   // mula.dd.*.ldinc mw, as, mx, my
                2 => vec![format!("m{}", (r >> 2) & 1), format!("m{}", 2 + ((t >> 2) & 1))],
                3 => vec![a(s), format!("m{}", 2 + ((t >> 2) & 1))],
                4 | 5 => vec![format!("m{}", r & 3), a(s), format!("m{}", (r >> 2) & 1), a(t)],
                6 => vec![format!("m{}", (r >> 2) & 1), a(t)],
                7 => vec![a(s), a(t)],
                8 | 9 => vec![format!("m{}", r & 3), a(s)],
                _ => vec![],
            }
        }
        Pie => vec![format!("0x{:x}", i.raw)],
    }
}

pub fn format(i: &Insn) -> String {
    let ops = operands(i);
    if ops.is_empty() { mnemonic(i) } else { format!("{} {}", mnemonic(i), ops.join(", ")) }
}
