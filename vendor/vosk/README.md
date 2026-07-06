# Native Vosk Library

This directory is intentionally kept out of source control except for this README.

Run one of:

```bash
./scripts/setup-vosk-macos.sh
```

```powershell
.\scripts\setup-vosk-windows.ps1
```

The scripts install `libvosk.dylib` or `vosk.dll` into `vendor/vosk/lib`, which is included in Cargo's native linker search path by `.cargo/config.toml`.
