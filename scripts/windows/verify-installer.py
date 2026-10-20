#!/usr/bin/env python3
"""Check the NSIS archive and compare its x64 payloads with the built release files."""
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile

root = Path(__file__).resolve().parents[2]
installer = Path(sys.argv[1]).resolve()
shell = root / 'apps/windows/src-tauri/target/x86_64-pc-windows-gnu/release'
sources = {
    'rustgit-windows.exe': shell / 'rustgit-windows.exe',
    'rgit.exe': root / 'target/x86_64-pc-windows-gnu/release/rgit.exe',
    'WebView2Loader.dll': shell / 'WebView2Loader.dll',
}
subprocess.run(['7zz', 't', str(installer)], check=True, stdout=subprocess.DEVNULL)
with tempfile.TemporaryDirectory(prefix='rgit-installer-verify-') as tmp:
    subprocess.run(['7zz', 'x', '-y', f'-o{tmp}', str(installer)], check=True, stdout=subprocess.DEVNULL)
    payloads = {}
    for name, source in sources.items():
        payload = Path(tmp) / name
        data, expected = payload.read_bytes(), source.read_bytes()
        normalization = None
        if name == 'rustgit-windows.exe':
            # Tauri's bundler changes this runtime marker for each package format, then restores
            # the original release executable. All other bytes must match exactly.
            marker = b'__TAURI_BUNDLE_TYPE_VAR_UNK'
            assert expected.count(marker) == 1, 'missing or ambiguous Tauri bundle marker'
            expected = expected.replace(marker, b'__TAURI_BUNDLE_TYPE_VAR_NSS')
            normalization = 'Tauri NSIS bundle marker UNK -> NSS'
        assert data == expected, f'{name} differs from its release binary'
        assert data[:2] == b'MZ', f'{name} is not a Windows executable'
        pe = struct.unpack_from('<I', data, 0x3c)[0]
        assert data[pe:pe + 4] == b'PE\x00\x00'
        assert struct.unpack_from('<H', data, pe + 4)[0] == 0x8664, f'{name} is not x64'
        if name == 'rustgit-windows.exe':
            assert struct.unpack_from('<H', data, pe + 24 + 68)[0] == 2, 'expected Windows GUI subsystem'
        imports = subprocess.check_output(['x86_64-w64-mingw32-objdump', '-p', str(payload)], text=True)
        dlls = sorted({line.split('DLL Name:')[1].strip() for line in imports.splitlines() if 'DLL Name:' in line})
        assert not any(dll.lower().startswith(('libgcc', 'libstdc++', 'libwinpthread')) for dll in dlls), dlls
        payloads[name] = {
            'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest(),
            'source_match': True, 'normalization': normalization, 'imports': dlls,
        }
    report = {
        'installer': str(installer), 'bytes': installer.stat().st_size,
        'sha256': hashlib.sha256(installer.read_bytes()).hexdigest(),
        'archive_integrity': '7zz t passed', 'payloads': payloads,
        'native_windows_install_launch': 'not run (macOS cross-build)',
    }
    (installer.parent / 'VERIFICATION.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({k: v for k, v in report.items() if k != 'payloads'}, ensure_ascii=False, indent=2))
    print('Verified payloads: ' + ', '.join(payloads))
