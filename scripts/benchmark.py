#!/usr/bin/env python3
"""Time fresh native processes or Docker containers against an already generated input."""
import argparse
import datetime
import json
import os
from pathlib import Path
import statistics
import subprocess
import tempfile
import time


def capture(*args):
    return subprocess.check_output(args, text=True).strip()


def elapsed_docker(stamp):
    return datetime.datetime.fromisoformat(stamp.replace("Z", "+00:00"))


def run_native(args, output, scratch):
    env = dict(os.environ, DATA_DIR=str(args.data), OUTPUT_DIR=str(output),
               SCRATCH_DIR=str(scratch), NCPU=str(args.cpus), PROFILE_STAGES="1")
    start = time.perf_counter()
    subprocess.run([args.binary], env=env, check=True)
    return {"process_wall_s": time.perf_counter() - start}


def run_docker(args, output, scratch):
    scratch.chmod(0o777)
    output.chmod(0o777)
    container = capture(
        "docker", "create", "--platform", "linux/amd64", "--network", "none",
        "--read-only", "--tmpfs", "/tmp:rw,size=64m,mode=1777", "--user", "1000:1000",
        "--cap-drop", "ALL", "--security-opt", "no-new-privileges", "--pids-limit", "4096",
        "--cpuset-cpus", f"0-{args.cpus - 1}", "--cpus", str(args.cpus),
        "--memory", args.memory, "--memory-swap", args.memory,
        "-v", f"{args.volume or args.data}:/data:ro", "-v", f"{output}:/output", "-v", f"{scratch}:/scratch",
        "-e", "DATA_DIR=/data", "-e", "OUTPUT_DIR=/output", "-e", "SCRATCH_DIR=/scratch",
        "-e", "HOME=/scratch", "-e", f"NCPU={args.cpus}", "-e", "PROFILE_STAGES=1",
        "-e", f"MEM_GB={args.memory[:-1]}", args.image,
    )
    try:
        subprocess.run(["docker", "start", "-a", container], check=True)
        state = json.loads(capture("docker", "inspect", "-f", "{{json .State}}", container))
        if state["ExitCode"] or state["OOMKilled"]:
            raise RuntimeError(f"exit {state['ExitCode']}, OOMKilled={state['OOMKilled']}")
        end = elapsed_docker(state["FinishedAt"])
        start = elapsed_docker(state["StartedAt"])
        return {"container_wall_s": (end - start).total_seconds()}
    finally:
        subprocess.run(["docker", "rm", "-f", container], check=True, stdout=subprocess.DEVNULL)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--binary", type=Path)
    mode.add_argument("--image")
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--data", type=Path)
    source.add_argument("--volume", help="Existing Docker volume containing the input dataset")
    parser.add_argument("--output", type=Path, required=True, help="Fresh directory for this series")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--cpus", type=int, default=8)
    parser.add_argument("--memory", default="24g", help="Container limit, e.g. 24g or 7g")
    args = parser.parse_args()
    if args.runs < 1 or args.cpus < 1:
        parser.error("runs and cpus must be positive")
    if args.volume:
        if args.binary:
            parser.error("--volume requires --image")
        subprocess.run(["docker", "volume", "inspect", args.volume], check=True, stdout=subprocess.DEVNULL)
    else:
        args.data = args.data.resolve(strict=True)
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    if args.binary:
        args.binary = args.binary.resolve(strict=True)
    times = []
    for index in range(args.runs):
        output = args.output / f"run-{index+1}"
        output.mkdir()
        with tempfile.TemporaryDirectory(prefix="shopping-scratch-") as temp:
            run = run_native if args.binary else run_docker
            result = run(args, output, Path(temp))
        times.append(next(iter(result.values())))
        print(json.dumps({"run": index+1, **result}), flush=True)
    print(json.dumps({"median_s": statistics.median(times), "runs": len(times)}), flush=True)


if __name__ == "__main__":
    main()
