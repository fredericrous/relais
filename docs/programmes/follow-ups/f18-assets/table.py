import json
rows=[r for r in json.load(open('rows.json')) if r.get('transcript')]
def cont(r):
    sig=[]
    if r.get('load1_mean') and r['load1_mean']>12: sig.append(f"load1 {r['load1_mean']:.0f}")
    if r['wfs_events']>=50: sig.append(f"{r['wfs_events']} slot waits")
    if r.get('slot_waits_s_own'): sig.append(f"own slot wait {r['slot_waits_s_own']:.0f}s")
    if r['concurrent']>=0.5: sig.append(f"{r['concurrent']:.1f} dispatches")
    return sig
K=['tail','suite','poll','late','model','other']
print('| run | worker | wall s | after DONE | suite | poll (late) | model | other | host at the time |')
print('|---|---|---|---|---|---|---|---|---|')
for r in sorted(rows,key=lambda r:r['run']):
    s=cont(r)
    print(f"| `{r['run'][4:17]}` | {r['worker']} | {r['wall']:.0f} | {r['tail']:.0f} | {r['suite']:.0f} | {r['poll']:.0f} ({r['late']:.0f}) | {r['model']:.0f} | {r['other']+r['spawn']:.0f} | {', '.join(s) or 'quiet'} |")
W=sum(r['wall'] for r in rows)
for grp,f in (('all',lambda r:True),('contended',lambda r:bool(cont(r))),('quiet',lambda r:not cont(r))):
    g=[r for r in rows if f(r)]; w=sum(r['wall'] for r in g)
    print(grp,len(g),round(w),{k:f"{sum(r[k] for r in g):.0f} ({100*sum(r[k] for r in g)/w:.0f}%)" for k in K+['nofile','spawn']})
