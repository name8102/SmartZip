"""Run from the repository root after cargo build -p smartzip-cli.

Requires 7z, zip, unrar and network for pinned libarchive data on first run.
All generated archives, databases, and raw reports stay under .work/.
"""
import binascii, hashlib, json, random, shutil, subprocess, urllib.request
from pathlib import Path
root=Path('.work/test-implementation').resolve();binary=Path('target/debug/smartzip').resolve()
root.mkdir(parents=True,exist_ok=True)
source=root/'source';source.mkdir(exist_ok=True)
rng=random.Random(20260905)
for i in range(3):(source/f'file{i+1}.bin').write_bytes(rng.randbytes(90*1024))
def run(args,**kw):return subprocess.run(args,capture_output=True,text=True,timeout=45,**kw)
for fmt,args in [('7z',['7z','a','-t7z','-m0=LZMA2','-ms=on','-v64k']),('zip',['zip','-0','-s','64k'])]:
 d=root/f'{fmt}-good';d.mkdir(exist_ok=True)
 if not list(d.iterdir()):
  dest=d/f'set.{fmt}'
  p=run(args+[str(dest)]+[str(p) for p in sorted(source.iterdir())]);assert p.returncode==0,p.stderr
commit='ddf8247381814977c2f55a59f48d17460f7d00f0'
d=root/'rar-good';d.mkdir(exist_ok=True)
for i in range(1,9):
 path=d/f'set.part{i:02}.rar'
 if path.exists():continue
 url=f'https://raw.githubusercontent.com/libarchive/libarchive/{commit}/libarchive/test/test_read_format_rar5_multiarchive.part{i:02}.rar.uu'
 data=urllib.request.urlopen(url,timeout=30).read().splitlines();started=False;decoded=[]
 for line in data:
  if line.startswith(b'begin '):started=True;continue
  if not started:continue
  if line==b'end':break
  if line:decoded.append(binascii.a2b_uu(line))
 path.write_bytes(b''.join(decoded))
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
rows=[]
for fmt in ['7z','zip','rar']:
 good=root/f'{fmt}-good'
 for case in ['good','flip-middle','flip-two','missing-middle','truncate-last','header-flip']:
  d=root/f'{fmt}-{case}'
  if case!='good':
   if d.exists():shutil.rmtree(d)
   shutil.copytree(good,d)
  volumes=sorted(d.iterdir(),key=lambda p:(p.suffix=='.zip',p.name))
  truth=[];missing=[]
  if case in ['flip-middle','flip-two']:
   for index in ([1] if case=='flip-middle' else [1, len(volumes)-2]):
    path=volumes[index];data=bytearray(path.read_bytes());offset=500 if fmt=='rar' else 6000;data[offset]^=0x5A;path.write_bytes(data);truth.append(path.name)
  if case=='missing-middle':missing=[volumes[1].name];volumes[1].unlink()
  if case=='truncate-last':path=volumes[-1];path.write_bytes(path.read_bytes()[:-80]);truth=[path.name]
  if case=='header-flip':path=volumes[1 if fmt=='rar' else 0];data=bytearray(path.read_bytes());data[12]^=0x5A;path.write_bytes(data);truth=[path.name]
  input_path=volumes[2] # arbitrary existing member, not necessarily first/final
  before={p.name:sha(p) for p in d.iterdir()}
  cmd=[str(binary),'--db',str(root/'history.db'),'t',str(input_path),'--json']
  p=run(cmd);report=json.loads(p.stdout);after={p.name:sha(p) for p in d.iterdir()};assert before==after
  r=report['files'][0];confirmed=[Path(v['path']).name for v in r['confirmed_volumes']];suspects=[[Path(x).name for x in g['members']] for g in r['suspect_groups']]
  assert set(confirmed)<=set(truth),(fmt,case,confirmed,truth)
  if fmt=='rar' and case in ['flip-middle','flip-two']:assert set(confirmed)==set(truth),(fmt,case,confirmed,truth)
  if truth:
   covered=set(confirmed)|{name for group in suspects for name in group}
   assert set(truth)<=covered or r['localization']=='unknown',(fmt,case,truth,covered)
  if missing:assert set(missing)<=set(Path(x).name for x in r['missing_volumes'])
  if case=='good':assert p.returncode==0 and r['integrity']=='intact',(fmt,r)
  else:assert p.returncode!=0,(fmt,case,r)
  row={'format':fmt,'case':case,'exit':p.returncode,'integrity':r['integrity'],'coverage':r['coverage'],'truth':truth,'confirmed':confirmed,'suspects':suspects,'missing':[Path(x).name for x in r['missing_volumes']],'stops':r['stop_reasons'],'passes':[(x['diagnostics']['family'],x['diagnostics']['failure']) for x in r['passes']], 'source_hashes_before':before, 'source_hashes_after':after, 'read_only_verified':True, 'mutation_offset':(500 if fmt=='rar' else 6000) if case.startswith('flip') else 12 if case=='header-flip' else None, 'truncated_bytes':80 if case=='truncate-last' else None};rows.append(row)
  (root/f'report-{fmt}-{case}.json').write_text(json.dumps(report,ensure_ascii=False,indent=2))
  print(json.dumps(row,ensure_ascii=False),flush=True)

# End-to-end password/history/exit checks use isolated databases.
import sqlite3
extra=[]
def cli_case(name,paths,options=(),expected=0):
 db=root/f'{name}.db'
 if db.exists():db.unlink()
 p=run([str(binary),'--db',str(db),'test',*[str(path) for path in paths],*options,'--json'])
 result=json.loads(p.stdout)
 assert p.returncode==expected==result['exit_code'],(name,p.stderr,result)
 conn=sqlite3.connect(db)
 records=conn.execute('SELECT damaged_volumes_json,test_report_json FROM file_extractions').fetchall()
 if '--no-history' in options:assert not records
 else:
  assert len(records)==len(result['files'])
  for row,report in zip(records,result['files']):
   assert json.loads(row[1])==report
   assert json.loads(row[0])==[volume['path'] for volume in report['confirmed_volumes']]
 assert conn.execute('SELECT COUNT(*) FROM known_files').fetchone()[0]==0
 extra.append({'case':name,'exit':p.returncode,'password_status':[r['password_status'] for r in result['files']], 'groups':len(result['files']), 'history_rows':len(records)})
 return result,conn
for fmt in ['7z','zip']:
 archive=root/f'encrypted.{fmt}'
 if archive.exists():archive.unlink()
 command=['7z','a',f'-t{fmt}','-pTestSecret-123']+(['-mhe=on'] if fmt=='7z' else ['-mem=AES256'])+[str(archive),str(source/'file1.bin')]
 p=run(command);assert p.returncode==0,p.stderr
 for label,options,expected,status in [('missing',[],1,'required'),('correct',['-p','TestSecret-123'],0,'verified'),('wrong',['-p','WrongSecret-456'],1,None)]:
  result,conn=cli_case(f'{fmt}-{label}',[archive],options,expected)
  if status:assert result['files'][0]['password_status']==status,result
  else:assert result['files'][0]['password_status'] in ['indeterminate','rejected']
  count=conn.execute('SELECT COUNT(*) FROM passwords WHERE success_count>0').fetchone()[0]
  assert count==(1 if label=='correct' else 0)
  conn.close()
result,conn=cli_case('mixed',[root/'7z-good/set.7z.003',root/'rar-flip-middle/set.part03.rar'],expected=2);conn.close()
result,conn=cli_case('deduplicated',[root/'7z-good/set.7z.001',root/'7z-good/set.7z.003']);assert len(result['files'])==1;conn.close()
result,conn=cli_case('no-history',[root/'zip-good/set.z02'],['--no-history']);conn.close()
result,conn=cli_case('no-empty-clear',[root/'zip-good/set.zip'],['--no-empty','-p','UnusedSecret-789']);assert result['files'][0]['password_status']!='verified';assert conn.execute('SELECT COUNT(*) FROM passwords').fetchone()[0]==0;conn.close()
versions={name:run(args).stdout.splitlines()[:6] for name,args in [('7z',['7z','i']),('unrar',['unrar']),('zip',['zip','-v'])]}
metadata={'seed':20260905,'libarchive_commit':commit,'versions':versions,'baseline_hashes':{fmt:{p.name:sha(p) for p in (root/f'{fmt}-good').iterdir()} for fmt in ['7z','zip','rar']},'cases':rows,'cli_checks':extra}
(root/'results.json').write_text(json.dumps(metadata,ensure_ascii=False,indent=2))
print(f'PASS: {len(rows)} volume mutations/baselines and {len(extra)} password/history/exit cases',flush=True)
