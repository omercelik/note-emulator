import json,collections,bisect
from pathlib import Path
root=Path(__file__).resolve().parent
r=json.loads((root/'profile-result.json').read_text())
syms=[]
for line in (root/'symbols.txt').read_text().splitlines():
 p=line.split(' ',2)
 if len(p)==3 and p[1] in ['T','t','W','w']:
  try:syms.append((int(p[0],16),p[2]))
  except ValueError:pass
syms.sort();addrs=[a for a,n in syms]
rows=[]
for log in r['logs']:
 for line in log.splitlines():
  p=line.split('\t')
  if len(p)!=7 or p[0]=='pc':continue
  pc=int(p[0],16);i=bisect.bisect_right(addrs,pc)-1
  rows.append(dict(pc=p[0],jit=p[1]=='true',samples=int(p[2]),insns=int(p[3]),ms=float(p[4]),missing=p[5],ops=p[6],symbol=syms[i][1] if i>=0 else '?'))
missing=collections.Counter();cost=collections.Counter();functions=collections.Counter()
for row in rows:
 if not row['jit']:
  missing[row['missing'] or '(eligible/cold/single)']+=row['insns']
  functions[row['symbol']]+=row['insns']
  cost[row['missing'] or '(eligible/cold/single)']+=row['ms']
print('Top rejected opcode sets (sampled instructions and sampled ms):')
for m,n in missing.most_common(25):print(n,round(cost[m],3),m)
print('Top interpreted blocks:')
for row in sorted([r for r in rows if not r['jit']],key=lambda r:-r['insns'])[:25]:print(row)
print('Top interpreted functions:')
for s,n in functions.most_common(15):print(n,s)
(root/'profile-rows.json').write_text(json.dumps(rows,indent=2))
