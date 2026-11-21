#!/usr/bin/env python3
"""Verify the reference against fixtures, or compare a pipeline with it on fresh seeds."""

import argparse
from collections import Counter
import os
from pathlib import Path
import subprocess
import tempfile

from reference import run
from run_fixtures import SCHEMAS, load_actual, load_expected


def compare(expected, actual, *, parquet):
    for table, schema in SCHEMAS.items():
        expected_table = load_expected(expected / f"{table}.csv", schema)
        actual_table = load_actual(actual / table, schema) if parquet else load_expected(actual / f"{table}.csv", schema)
        want = Counter(expected_table.select(schema.keys()).rows())
        got = Counter(actual_table.select(schema.keys()).rows())
        if want != got:
            missing, extra = want - got, got - want
            raise ValueError(
                f"{table}: {sum(missing.values())} missing, {sum(extra.values())} extra; "
                f"sample missing {list(missing)[:2]}, sample extra {list(extra)[:2]}"
            )


def fixtures(path):
    cases = sorted(case for case in path.iterdir() if case.is_dir())
    if not cases:
        raise ValueError(f"no fixtures found under {path}")
    for case in cases:
        data = case / "input" if (case / "input").is_dir() else case / "data"
        with tempfile.TemporaryDirectory() as temp:
            actual = Path(temp)
            run(data, actual)
            try:
                compare(case / "expected", actual, parquet=False)
            except ValueError as error:
                raise ValueError(f"{case.name}: {error}") from error
    print(f"Reference matched {len(cases)} fixtures.")


def generated(args):
    for seed in args.seeds:
        with tempfile.TemporaryDirectory(prefix=f"shopping-seed-{seed}-") as temp:
            base = Path(temp)
            data, expected, actual, scratch = (base / sub for sub in ("input", "csv", "actual", "scratch"))
            actual.mkdir()
            scratch.mkdir()
            subprocess.run([
                args.generator, "--seed", str(seed), "--events", str(args.events),
                "--clicks", str(args.clicks), "--output", data,
            ], check=True)
            run(data, expected)
            env = dict(os.environ, DATA_DIR=str(data), OUTPUT_DIR=str(actual), SCRATCH_DIR=str(scratch))
            subprocess.run([args.pipeline, data, actual], env=env, check=True)
            try:
                compare(expected, actual, parquet=True)
            except ValueError as error:
                raise ValueError(f"seed {seed}: {error}") from error
            print(f"Seed {seed} matched ({args.events} events and {args.clicks} requested clicks).", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    fx = sub.add_parser("fixtures", help="compare the reference with the organizer's expected CSVs")
    fx.add_argument("directory", type=Path)
    gen = sub.add_parser("seeds", help="generate fresh inputs and compare both pipeline Parquet tables")
    gen.add_argument("--generator", type=Path, required=True)
    gen.add_argument("--pipeline", type=Path, required=True)
    gen.add_argument("--events", type=int, default=5000)
    gen.add_argument("--clicks", type=int, default=1300)
    gen.add_argument("--seeds", nargs="+", type=int, default=[2, 17, 103])
    args = parser.parse_args()
    if args.action == "fixtures":
        fixtures(args.directory)
    else:
        args.generator = args.generator.resolve(strict=True)
        args.pipeline = args.pipeline.resolve(strict=True)
        generated(args)
