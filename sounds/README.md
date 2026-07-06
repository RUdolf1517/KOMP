# KOMP sounds

Put MP3, WAV, or OGG response files here.

System sounds live in `sounds/system/`:

- `startup.mp3` - played when KOMP starts.
- `shutdown.mp3` - played when KOMP shuts down or stops listening.
- `listening.mp3` - played after the wake phrase and before command capture.
- `hello.mp3` - played by the built-in hello scenario.
- `power_connected.mp3|wav|ogg` - optional, played when charging is connected.
- `power_disconnected.mp3|wav|ogg` - optional, played when charging is disconnected.
- `battery_unavailable.mp3|wav|ogg` - optional, played when battery status cannot be read.
- `battery_0_10.mp3|wav|ogg`
- `battery_10_20.mp3|wav|ogg`
- `battery_20_30.mp3|wav|ogg`
- `battery_30_40.mp3|wav|ogg`
- `battery_40_50.mp3|wav|ogg`
- `battery_50_60.mp3|wav|ogg`
- `battery_60_70.mp3|wav|ogg`
- `battery_70_80.mp3|wav|ogg`
- `battery_80_90.mp3|wav|ogg`
- `battery_90_100.mp3|wav|ogg`
- `battery_100.mp3|wav|ogg`

KOMP plays battery level prompts when the user asks for battery status and also after charger connect/disconnect events. Old `battery_gt_50.mp3` style files still work as fallback.

Configure global sounds in `komp.toml`:

```toml
[sounds]
startup = "sounds/system/startup.mp3"
shutdown = "sounds/system/shutdown.mp3"
wake = "sounds/system/listening.mp3"
listening = "sounds/system/listening.mp3"
```

Configure scenario sounds in plugin TOML files:

```toml
[[scenarios.steps]]
id = "ack"
action = { type = "play_sound", file = "sounds/system/listening.mp3" }
```
