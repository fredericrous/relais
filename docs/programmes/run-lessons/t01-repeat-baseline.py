import sqlite3,os,re,json,glob,sys,collections
from datetime import datetime,timedelta
S=os.path.expanduser('~/.local/state/relais'); W=timedelta(days=14)
c=sqlite3.connect(f'file:{S}/ledger.sqlite?mode=ro',uri=True)
def repo(p): return re.sub(r'-wt-.*$','',os.path.basename(p.rstrip('/')))
def ts(s): return datetime.fromisoformat(s.replace('Z','+00:00'))
runs={r:(repo(p),ts(t),st) for r,p,t,st in c.execute("select id,repo_path,created_at,status from runs")}
rc=collections.defaultdict(collections.Counter)
for r,(rp,_,_) in runs.items(): rc[rp]['runs']+=1
# known rule ids from adr-lint rulings
rules=set(); rul=[]
for f in glob.glob(f'{S}/runs/*/rulings.jsonl'):
    r=f.split('/')[-2]
    for l in open(f):
        try: j=json.loads(l)
        except: continue
        rules.add(j['rule']); rul.append((r,j['rule'],j.get('pattern_id'),j.get('ruling')))
FILE=re.compile(r'[\w./-]+\.(?:rs|ts|tsx|py|md|toml|json|ya?ml|sh|css|sql)')
RULE=re.compile(r'[a-z]+(?:\.[a-z][a-z0-9-]+)+')
REV=set()
fnd=[]  # (run,repo,time,files,rules,words)
for f in glob.glob(f'{S}/runs/*/review-*.disp-*.txt')+glob.glob(f'{S}/runs/*/review.txt'):
    r=f.split('/')[-2]
    if r not in runs: continue
    t=open(f,errors='ignore').read(); m=re.search(r'^FINDINGS:\s*(.*)',t,re.M|re.S)
    if not m: continue
    REV.add(r)
    for b in re.split(r'\n(?=- )',m.group(1)):
        if not b.startswith('- '): continue
        h=b[:300]; fl={x.split(':')[0] for x in FILE.findall(h)}
        fnd.append((r,runs[r][0],runs[r][1],fl,{x for x in RULE.findall(b) if x in rules},{w for w in re.findall(r'[a-z]{4,}',h.lower())}))
for r in REV: rc[runs[r][0]]['reviewed']+=1
fnd.sort(key=lambda x:x[2])
def rep(a,b):
    if a[0]==b[0] or a[1]!=b[1] or not(timedelta(0)<(b[2]-a[2])<=W) or not(a[3]&b[3]): return False
    if a[4]&b[4]: return 'rule+file'
    j=len(a[5]&b[5])/max(1,len(a[5]|b[5]))
    return 'text+file' if j>=.5 else False
ex=[]
for i,b in enumerate(fnd):
    rc[b[1]]['findings']+=1
    k=next((rep(a,b) for a in fnd[:i] if rep(a,b)),False)
    if k:
        rc[b[1]]['f_repeat']+=1; rc[b[1]]['f_'+k.replace('+','_')]+=1
        if len(ex)<400: ex.append((b[1],k,b[0],sorted(b[3])[:2],sorted(b[4])))
# adr-lint rulings: same rule+pattern in same repo within 14d, earlier other run
seen=collections.defaultdict(list)
for r,ru,pa,rl in sorted(rul,key=lambda x:runs[x[0]][1] if x[0] in runs else datetime.min):
    if r not in runs: continue
    rp,t,_=runs[r]; rc[rp]['rulings']+=1; rc[rp]['nonfine']+=(rl!='fine')
    if any(r2!=r and timedelta(0)<(t-t2)<=W for r2,t2 in seen[(rp,ru,pa)]): rc[rp]['ruling_repeat']+=1; rc[rp]['nonfine_repeat']+=(rl!='fine')
    seen[(rp,ru,pa)].append((r,t))
# repaired failures
ev=[]
for r,d,at in c.execute("select run_id,detail_json,at from transitions where reason='behavioral_failure' order by id"):
    if r not in runs: continue
    try: labs=json.loads(d).get('failures',[])
    except: continue
    order=[x[0] for x in c.execute("select label from check_timings where run_id=? and phase='attempt1' order by queued_at",(r,))]
    for lb in labs:
        line=''
        if lb in order:
            p=f'{S}/runs/{r}/logs/attempt1-cmd{order.index(lb)}.log'
            if os.path.exists(p):
                for l in open(p,errors='ignore'):
                    if re.search(r'error|FAILED|AssertionError|panicked|✗|FAIL\b',l) and ' ... ok' not in l:
                        line=re.sub(r'\d+|/\S+','#',l.strip())[:120]; break
        ev.append((r,runs[r][0],ts(at),lb,line,runs[r][2] in('accepted','accepted_by_person')))
ex2=[]
for i,b in enumerate(ev):
    rc[b[1]]['fail_ev']+=1; rc[b[1]]['fail_repaired']+=b[5]
    for a in ev[:i]:
        if a[0]!=b[0] and a[1]==b[1] and a[5] and a[3]==b[3] and timedelta(0)<(b[2]-a[2])<=W:
            rc[b[1]]['fail_repeat_label']+=1
            if a[4] and a[4]==b[4]:
                rc[b[1]]['fail_repeat_label_line']+=1; ex2.append((b[1],b[3],b[4],a[0],b[0]))
            break
K=['runs','reviewed','findings','f_repeat','f_rule_file','f_text_file','rulings','ruling_repeat','nonfine','nonfine_repeat','fail_ev','fail_repaired','fail_repeat_label','fail_repeat_label_line']
print('repo',*K,sep='\t')
tot=collections.Counter()
for rp,v in sorted(rc.items(),key=lambda x:-x[1]['runs']):
    if v['runs']>=20 or v['findings']: print(rp,*[v[k] for k in K],sep='\t')
    for k in K: tot[k]+=v[k]
print('ALL',*[tot[k] for k in K],sep='\t')
if '-x' in sys.argv:
    for e in ex[:12]: print('F',e)
    for e in ex2[:12]: print('X',e)
