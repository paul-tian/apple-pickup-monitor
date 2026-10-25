#!/usr/bin/env python3
"""Check packaged translations retain every key and format placeholder."""
import json
from pathlib import Path
import re
import subprocess

root = Path(__file__).resolve().parent.parent / "native"
tables = {}
for path in sorted(root.glob("*.lproj/Localizable.strings")):
    result = subprocess.run(
        ["plutil", "-convert", "json", "-o", "-", str(path)],
        check=True, capture_output=True, text=True,
    )
    tables[path.parent.stem] = json.loads(result.stdout)

if "en" not in tables or "zh-Hans" not in tables:
    raise SystemExit("English and Simplified Chinese translation tables are required")

format_pattern = re.compile(r"%(?:\d+\$)?[-+#0 ]*(?:\d+|\*)?(?:\.(?:\d+|\*))?(?:hh|h|ll|l|z|t|j|L)?[@diuoxXfFeEgGaAcsp%]")

def formats(value):
    return sorted(re.sub(r"^%\d+\$", "%", item) for item in format_pattern.findall(value) if item != "%%")

english = tables["en"]
reference_pattern = re.compile(r'L10n\.(?:text|format)\("((?:\\.|[^"\\])*)"')
referenced = set()
for source in root.glob("*.swift"):
    for literal in reference_pattern.findall(source.read_text()):
        if not literal:
            continue
        try:
            referenced.add(json.loads('"' + literal + '"'))
        except json.JSONDecodeError:
            # Interpolated Swift strings are checked through their format keys.
            continue
missing_references = sorted(referenced - english.keys())
if missing_references:
    raise SystemExit(f"English table is missing referenced keys: {missing_references}")

for language, table in tables.items():
    if table.keys() != english.keys():
        missing = sorted(english.keys() - table.keys())
        extra = sorted(table.keys() - english.keys())
        raise SystemExit(f"{language}: missing keys {missing}; extra keys {extra}")
    for key, value in table.items():
        if not isinstance(value, str) or not value.strip():
            raise SystemExit(f"{language}: empty translation for {key!r}")
        if formats(value) != formats(english[key]):
            raise SystemExit(f"{language}: format placeholders differ for {key!r}")

print(f"Localization tables passed: {len(english)} keys in {', '.join(tables)}")
