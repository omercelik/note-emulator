# gdb python: single-step N instructions on the halted target, log pc + a0..a15 + ps + windowbase
import gdb, sys
N=int(gdb.parse_and_eval("$steps")) if gdb.convenience_variable("steps") is not None else 20000
out=open(str(gdb.parse_and_eval("$outfile")).strip('"'),"w")
regs=['a%d'%i for i in range(16)]
for i in range(N):
    pc=int(gdb.parse_and_eval("$pc")) & 0xffffffff
    vals=[int(gdb.parse_and_eval("$"+r)) & 0xffffffff for r in regs]
    ps=int(gdb.parse_and_eval("$ps")) & 0xffffffff
    wb=int(gdb.parse_and_eval("$windowbase")) & 0xffffffff
    out.write("%08x %s %08x %x\n" % (pc, " ".join("%08x"%v for v in vals), ps, wb))
    gdb.execute("stepi", to_string=True)
out.close()
print("done", N)
