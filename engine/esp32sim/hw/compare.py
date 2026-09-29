#!/usr/bin/env python3
"""Compare a hardware single-step trace (gdb) with an emulator trace.
Both files: one line per executed instruction:  <pc> <a0> <a1> ... <a15> <ps> <wb>   (hex, no 0x)
Reports the first PC divergence and register mismatches (with a small context)."""
import sys
def load(p):
    out=[]
    for line in open(p):
        t=line.split()
        if len(t)<19: continue
        try: out.append([int(x,16) for x in t[:19]])
        except ValueError: continue
    return out
hw=load(sys.argv[1]); em=load(sys.argv[2])
# JTAG single-step executes window overflow/underflow exceptions atomically (same PC reported again);
# the emulator executes the handlers instruction by instruction. Normalise both.
VEC=0x40000000
em=[r for r in em if not (VEC<=r[0]<VEC+0x180)]
def collapse(t):
    o=[]
    for r in t:
        if o and o[-1][0]==r[0]: continue   # re-executed instruction after an exception
        o.append(r)
    return o
hw=collapse(hw); em=collapse(em)
names=['pc']+[f'a{i}' for i in range(16)]+['ps','wb']
n=min(len(hw),len(em)); print(f"hw {len(hw)} steps, emu {len(em)} steps, comparing {n}")
regdiff=0; resyncs=0
i=j=0
while i<len(hw) and j<len(em):
    h,e=hw[i],em[j]
    if h[0]!=e[0]:
        # timing loops (ets_delay_us & co) iterate a different number of times under single-step:
        # try to resynchronise by skipping ahead on whichever side is still looping.
        def find(trace,start,pcs,limit=200000):
            for k in range(start,min(len(trace),start+limit)):
                if all(k+m<len(trace) and trace[k+m][0]==pcs[m] for m in range(len(pcs))): return k
            return None
        want=[r[0] for r in hw[i:i+4]]
        k=find(em,j,want)
        if k is None:
            want=[r[0] for r in em[j:j+4]]; k2=find(hw,i,want)
            if k2 is None:
                print(f"PC DIVERGES at hw step {i} / emu step {j}: hw {h[0]:08x} vs emu {e[0]:08x}")
                for m in range(max(0,i-6), i+1): print(f"  hw[{m}] {hw[m][0]:08x}   emu[{j-(i-m)}] {em[j-(i-m)][0]:08x}" if 0<=j-(i-m)<len(em) else f"  hw[{m}] {hw[m][0]:08x}")
                break
            print(f"resync: hardware skipped ahead {k2-i} steps at hw step {i} (pc {h[0]:08x}) -> {hw[k2][0]:08x}"); resyncs+=1; i=k2; continue
        print(f"resync: emulator ran {k-j} extra steps at emu step {j} (pc {e[0]:08x}) -> {em[k][0]:08x}"); resyncs+=1; j=k; continue
    def norm(k,v): return v & ~0xf if names[k]=='ps' else v
    d=[(names[k],h[k],e[k]) for k in range(1,19) if norm(k,h[k])!=norm(k,e[k]) and not (i==0 and names[k]=='ps')]
    if d and regdiff<15:
        regdiff+=1
        print(f"step {i} pc {h[0]:08x}: "+", ".join(f"{nm} hw={hv:08x} emu={ev:08x}" for nm,hv,ev in d))
    i+=1; j+=1
else:
    print(f"no PC divergence: compared {i} hw steps ({resyncs} timing resyncs)")
