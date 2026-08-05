#!/usr/bin/env python3
"""Fetch additional 12306 ZIP files and check their contents"""
import subprocess
import zipfile
import sys

# Additional UIDs to check
uids_to_check = [614, 620]

# Load environment and fetch emails
env_setup = "export PATH='/home/holo/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH' && export $(grep -v '^#' .env.local | xargs)"

print("Fetching additional 12306 ZIP samples...\n")

for uid in uids_to_check:
    print(f"=== UID {uid} ===")

    # Use Rust program to fetch this specific UID
    # We'll create a simpler version that just fetches one UID
    cmd = f"""
cd /home/holo/work-tools && {env_setup} &&
cargo run --release -- audit 879455187@qq.com 2>&1 | grep "^{uid}\\t"
"""

    result = subprocess.run(cmd, shell=True, capture_output=True, text=True, executable='/bin/bash')

    if result.stdout:
        parts = result.stdout.strip().split('\t')
        if len(parts) >= 5:
            filename = parts[4]
            print(f"Filename: {filename}")

    print()
