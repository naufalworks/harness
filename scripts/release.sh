#!/usr/bin/env bash
# Build deterministic release evidence: binary, CycloneDX SBOM, manifest and checksums.
set -euo pipefail
cd "$(dirname "$0")/.."
for command in cargo git python3 rustc sha256sum tar gzip; do
  command -v "$command" >/dev/null || { echo "BLOCKED: $command is required" >&2; exit 2; }
done

out=${HARNESS_RELEASE_DIR:-dist}
target=${HARNESS_RELEASE_TARGET:-$(rustc -vV | sed -n 's/^host: //p')}
version=$(python3 - <<'PY'
import tomllib
print(tomllib.load(open('Cargo.toml','rb'))['package']['version'])
PY
)
epoch=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}
name="harness-${version}-${target}"
stage="$out/.stage-$name"
rm -rf "$stage"
mkdir -p "$stage/$name"

if [ "${HARNESS_RELEASE_SKIP_BUILD:-0}" != 1 ]; then
  build_args=(--locked --release)
  if [ -n "${HARNESS_RELEASE_TARGET:-}" ]; then
    build_args+=(--target "$target")
  fi
  cargo build "${build_args[@]}"
fi
if [ -n "${HARNESS_RELEASE_TARGET:-}" ]; then
  binary="target/$target/release/harness"
else
  binary="target/release/harness"
fi
[ -x "$binary" ] || { echo "BLOCKED: release binary missing at $binary" >&2; exit 2; }
install -m 0755 "$binary" "$stage/$name/harness"
install -m 0644 README.md Cargo.lock "$stage/$name/"

python3 - "$stage/$name/sbom.cdx.json" "$name" "$version" <<'PY'
import json, sys, tomllib
from pathlib import Path
lock=tomllib.loads(Path('Cargo.lock').read_text())
components=[]
for package in sorted(lock.get('package', []), key=lambda p:(p['name'],p['version'])):
    item={'type':'library','name':package['name'],'version':package['version'],
          'bom-ref':f"pkg:cargo/{package['name']}@{package['version']}"}
    if 'checksum' in package:
        item['hashes']=[{'alg':'SHA-256','content':package['checksum']}]
    components.append(item)
doc={'bomFormat':'CycloneDX','specVersion':'1.5','serialNumber':'urn:uuid:00000000-0000-0000-0000-000000000000','version':1,
     'metadata':{'component':{'type':'application','name':sys.argv[2],'version':sys.argv[3]}},'components':components}
Path(sys.argv[1]).write_text(json.dumps(doc,sort_keys=True,separators=(',',':'))+'\n')
PY
(
  cd "$stage/$name"
  sha256sum harness README.md Cargo.lock sbom.cdx.json > MANIFEST.sha256
)
python3 - "$stage/$name/release.json" "$name" "$target" "$(git rev-parse HEAD)" "$epoch" <<'PY'
import json,sys
from pathlib import Path
Path(sys.argv[1]).write_text(json.dumps({'name':sys.argv[2],'target':sys.argv[3],'commit':sys.argv[4],'source_date_epoch':int(sys.argv[5])},sort_keys=True,separators=(',',':'))+'\n')
PY
find "$stage" -exec touch -h -d "@$epoch" {} +
mkdir -p "$out"
archive="$out/$name.tar.gz"
TZ=UTC tar --sort=name --format=ustar --owner=0 --group=0 --numeric-owner --mtime="@$epoch" -C "$stage" -cf - "$name" | gzip -n -9 > "$archive"
sha256sum "$archive" > "$archive.sha256"
rm -rf "$stage"
echo "release artifact: $archive"
echo "SBOM: $name/sbom.cdx.json; checksum: $archive.sha256"
echo "[INFO] cryptographic signature is produced by the networked release CI with cosign; this local script does not claim one."
