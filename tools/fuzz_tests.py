"""Bounded coverage-guided runs; retains only the public-input corpus/logs."""
import json
from pathlib import Path
import os
import re
import struct
import subprocess

ROOT=Path(__file__).resolve().parents[1]
report=ROOT/"test-results/fuzz"
report.mkdir(parents=True,exist_ok=True)
corpus=ROOT/"fuzz/corpus/control"
corpus.mkdir(parents=True,exist_ok=True)
def frame(kind,body):
    return struct.pack("<IHHI",8+len(body),kind,0,1)+body
seeds={"hello":frame(1,b"SSRP"+struct.pack("<HH",3,0)),"ack":frame(16,b""),
       "parameters":frame(20,struct.pack("<IH",0,0)),"image":frame(28,struct.pack("<II",1,1)+bytes([32,64,128,255])),
       "state":b"SSV3\1"+struct.pack("<I",2)+b"ab"+struct.pack("<I",2)+b"cd"}
for name,data in seeds.items(): (corpus/name).write_bytes(data)
seconds=int(os.environ.get("VST_AUDIO_FUZZ_SECONDS","120"))
results=[]
try:
    for target in ["control","ring"]:
        command=["cargo","+nightly-2026-10-20","fuzz","run",target,"--","-max_total_time="+str(seconds),"-max_len=65536","-rss_limit_mb=2048","-print_final_stats=1"]
        result=subprocess.run(command,cwd=ROOT,capture_output=True,text=True,timeout=seconds+300)
        output=result.stdout+result.stderr
        (report/(target+".log")).write_text(output,encoding="utf-8")
        print(output[-12000:],flush=True)
        units=re.search(r"stat::number_of_executed_units:\s*(\d+)",output)
        executions=int(units.group(1)) if units else 0
        passed=result.returncode==0 and executions>100
        results.append({"target":target,"seconds":seconds,"executions":executions,"passed":passed})
        if not passed and result.returncode==0: raise RuntimeError("fuzzer did not execute enough inputs")
        if result.returncode: raise SystemExit(result.returncode)
finally:
    (report/"summary.json").write_text(json.dumps(results,indent=2))
