#!/usr/bin/env python3
"""Verify real CLI extraction, original-name mapping, independent SHA-256 and DB reports."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import sqlite3
import subprocess
import tempfile
import zipfile


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output-report", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve()
    output_report = args.output_report.resolve()
    output_report.parent.mkdir(parents=True, exist_ok=True)
    fixture = output_report.with_suffix(".zip")
    directory = "共享中文目录" * 80
    names = [f"{directory}/{'中文' * 170}.txt", f"{directory}/{'👨‍👩‍👧‍👦' * 60}.bin",
             "Case.txt", "case.txt", "é.txt", "e\u0301.txt", "CON.txt", "bad:name?.txt",
             "tail. ", "extension." + "扩展" * 150, "empty/"]
    originals = {name: (f"unique member {i}: 完整内容\n" * (i + 1)).encode() if not name.endswith("/") else b""
                 for i, name in enumerate(names)}
    with zipfile.ZipFile(fixture, "w", compression=zipfile.ZIP_STORED) as archive:
        for name, content in originals.items():
            info = zipfile.ZipInfo(name, date_time=(2024, 1, 2, 3, 4, 6))
            info.create_system = 3
            info.external_attr = (0o40755 if name.endswith("/") else 0o100644) << 16
            archive.writestr(info, content)
    evidence = {"fixture": fixture.name, "fixture_sha256": sha256(fixture.read_bytes()),
                "platform": platform.platform(), "archive_entries": len(originals), "runs": []}
    with tempfile.TemporaryDirectory(prefix="sz-path-") as scratch:
        root = Path(scratch)
        database = root / "state.sqlite"
        config = root / "config.toml"
        config.write_text("schema_version=1\n[state]\nmode='read-write'\ndatabase=" + json.dumps(str(database)) +
                          "\n[interaction]\nmode='never'\n[extraction.recursion]\nenabled=false\n"
                          "[extraction.embedded]\nroot='off'\nnested='off'\n[extraction.output]\nlayout='raw'\non_conflict='overwrite'\n",
                          encoding="utf-8")
        for mode in ("portable", "native"):
            output = root / mode
            command = [str(binary), "--config", str(config), "extract", str(fixture), "--output", str(output),
                       "--path-mode", mode, "--force", "--json", "--non-interactive"]
            result = subprocess.run(command, capture_output=True, text=True, check=False, timeout=120)
            if result.returncode != 0:
                raise RuntimeError(f"{mode}: exit={result.returncode}\n{result.stdout}\n{result.stderr}")
            payload = json.loads(result.stdout)
            assert payload["failed_count"] == 0
            report, = payload["path_reports"]
            assert not report["tentative"] and len(report["entries"]) == len(originals)
            verified = []
            for entry in report["entries"]:
                actual = output / entry["final_relative"]
                assert entry["source"] in originals
                assert bytes(entry["raw_name"]) == entry["source"].encode("utf-8")
                if entry["is_dir"]:
                    assert actual.is_dir()
                    verified.append({"id": entry["id"], "directory_present": True})
                else:
                    checksum = sha256(actual.read_bytes())
                    assert checksum == sha256(originals[entry["source"]])
                    verified.append({"id": entry["id"], "bytes": len(originals[entry["source"]]), "sha256": checksum})
            with sqlite3.connect(database) as connection:
                stored_json, commit_json = connection.execute(
                    "SELECT path_report_json,commit_json FROM file_extractions WHERE task_id=? AND path_report_json IS NOT NULL",
                    (payload["task_id"],)).fetchone()
                stored = json.loads(stored_json)
                assert stored == report
                intent = json.loads(commit_json)["intent"]
                assert intent["mapping_digest"] == report["digest"]
                assert intent["path_mapping"] == report
            evidence["runs"].append({"mode": mode, "status": payload["status"], "verified_contents": verified,
                                     "db_report_matches": True, "commit_digest_matches": True, "mapping": report})
        output = root / "native"
        before = {str(path.relative_to(output)): sha256(path.read_bytes()) for path in output.rglob("*") if path.is_file()}
        with config.open("a", encoding="utf-8") as handle:
            handle.write("\n[limits]\nmax_output_bytes=8\n")
        failed = subprocess.run([str(binary), "--config", str(config), "extract", str(fixture), "--output", str(output),
                                 "--path-mode", "native", "--force", "--json", "--non-interactive"],
                                capture_output=True, text=True, check=False, timeout=120)
        assert failed.returncode == 1, failed.stdout + failed.stderr
        payload = json.loads(failed.stdout)
        assert payload["failed_count"] == 1
        report, = payload["path_reports"]
        assert report["tentative"]
        after = {str(path.relative_to(output)): sha256(path.read_bytes()) for path in output.rglob("*") if path.is_file()}
        assert before == after
        assert not list(output.glob(".smartzip-*"))
        with sqlite3.connect(database) as connection:
            stored_json, = connection.execute("SELECT path_report_json FROM file_extractions WHERE task_id=? AND path_report_json IS NOT NULL",
                                               (payload["task_id"],)).fetchone()
            assert json.loads(stored_json) == report
        evidence["budget_failure"] = {"old_output_unchanged": True, "staging_cleaned": True,
                                      "db_tentative_report_matches": True, "mapping_digest": report["digest"]}
    output_report.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"archive_entries": len(originals), "content_files_per_run": len(originals) - 1,
                      "modes": [run["mode"] for run in evidence["runs"]], "db_report_matches": True,
                      "report": str(output_report)}, ensure_ascii=False))


if __name__ == "__main__":
    main()
