#!/usr/bin/env python3
"""Run a clean-target restore drill for one encrypted Harness backup."""
import argparse
from backup import restore_drill


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive")
    parser.add_argument("--key-file", required=True)
    args = parser.parse_args()
    if not restore_drill(args.archive, args.key_file):
        raise SystemExit("Restore drill failed")
    print("Restore drill passed")


if __name__ == "__main__":
    main()
