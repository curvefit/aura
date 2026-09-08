#!/usr/bin/env python3
"""Reproduce the bounded remaining probes on the frozen local corpus.

Acquires the canonical benchmark lock and all four existing encoding slots;
lock descriptors are inherited by children. This does not control collectors.
"""
import fcntl,os,pathlib,json,subprocess,time,datetime
R=pathlib.Path('/home/anton/Downloads/lean-node-20260907'); D=R/'results-w03/final-probes';D.mkdir(exist_ok=True)
B=R/'bin-candidate'; C=R/'corpus-w03/current'; jobs=[]
def run(name,args):
    print('START',name,datetime.datetime.now(datetime.timezone.utc).isoformat(),flush=True)
    started=time.monotonic_ns()
    with (D/(name+'.stdout')).open('wb') as out,(D/(name+'.stderr')).open('wb') as err:
        proc=subprocess.Popen([str(x) for x in args],stdout=out,stderr=err,pass_fds=tuple(fds))
        _,status,ru=os.wait4(proc.pid,0);proc.returncode=os.waitstatus_to_exitcode(status)
    row={'name':name,'command':[str(x) for x in args],'returncode':proc.returncode,'wall_ns':time.monotonic_ns()-started,'cpu_user_s':ru.ru_utime,'cpu_system_s':ru.ru_stime,'peak_rss_kib':ru.ru_maxrss,'finished_utc':datetime.datetime.now(datetime.timezone.utc).isoformat()}
    jobs.append(row);(D/'commands.json').write_text(json.dumps(jobs,indent=2)+'\n');print('DONE',name,proc.returncode,round(row['wall_ns']/1e9,3),flush=True)
    if proc.returncode:raise RuntimeError(name+' failed; see stderr')
fds=[]
try:
    fd=os.open('/tmp/lean-node-20260907-1000/bench.lock',os.O_RDONLY);fds.append(fd);fcntl.flock(fd,fcntl.LOCK_EX)
    for i in range(4):
        fd=os.open(f'/media/anton/data/grimoire-home/encoder-slots/{i}.lock',os.O_RDONLY|os.O_NOFOLLOW);fds.append(fd);fcntl.flock(fd,fcntl.LOCK_EX)
    (D/'conditions.json').write_text(json.dumps({'started_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'lock':'/tmp/lean-node-20260907-1000/bench.lock','encoding_slots':'all four existing Grimoire slots held; codec jobs drained naturally; collectors unchanged','threads':1,'loadavg':pathlib.Path('/proc/loadavg').read_text().strip()},indent=2)+'\n')
    run('candles-prepare',['python3',R/'aura-w03/docs/campaigns/lean-node-20260907/w03-transmute/prepare_candles.py'])
    candles=R/'corpus-w03/candles'
    for name in ['real-candles-es-16384','real-candles-nq-16384','real-candles-es-tiny16']:
        run(name+'-encode',[B/'aura-json-i64','--schema','100,0,0,0,0,5,5,5,0,0','--timestamp-multiplier','1','--decimal-scale','1','--out',candles/(name+'.aura'),candles/(name+'.input.json')])
    run('bitget-zstd19',[B/'aura-bench','--operation','zstd-aura1-to-aura1-bytes','--dataset','bitget_delta','--input',C/'bitget_delta.aura1','--reference-aura0',C/'bitget_delta.aura0','--reference-aura1',C/'bitget_delta.aura1','--zstd-level','19','--iterations','3','--warmups','1','--output',D/'bitget-zstd19.json'])
    run('structural-pair',[B/'structural_frontier',C/'bitget_delta.aura0',D/'structural'])
    for name in ['packed','huffman']:
        a0=D/'structural'/(name+'.aura0');a1=a0.with_suffix('.aura1')
        run(name+'-emit',[B/'aura-bench','--operation','transcode-aura0-to-aura1','--input',a0,'--dataset',name,'--unsupported-path','fallback-to-stable','--iterations','1','--warmups','0','--preserve-output',a1,'--verify-output-decodes','--output',D/(name+'-emit.json')])
        run(name+'-fair',[B/'aura-bench','--operation','aura0-to-aura1-bytes','--input',a0,'--reference-aura0',a0,'--reference-aura1',a1,'--dataset',name,'--unsupported-path','fallback-to-stable','--iterations','3','--warmups','1','--output',D/(name+'-fair.json')])
    run('bitget-profile',[B/'transmutation_probe','--input',C/'bitget_delta.aura0','--output-dir',D/'bitget-profile','--iterations','1','--warmups','0','--profile-stages','--file-io'])
    for name in ['real-candles-es-16384','real-candles-nq-16384','real-candles-es-tiny16']:
        run(name+'-probe',[B/'transmutation_probe','--input',candles/(name+'.aura0'),'--output-dir',D/name,'--iterations','3','--warmups','1','--file-io'])
finally:
    for fd in reversed(fds):os.close(fd)
