# Erez sounds

Put MP3, WAV, or OGG response files here.

System sounds live in `sounds/system/`:

- `startup.mp3` - played when KOMP starts.
- `shutdown.mp3` - played when KOMP shuts down or stops listening.
- `listening.mp3` - played after the wake phrase and before command capture.
- `hello.mp3` - played by the built-in hello scenario.

Configure global sounds in `erez.toml`:

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
