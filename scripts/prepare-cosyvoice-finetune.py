#!/usr/bin/env python3
"""Convert KOMP's paired WAV/TXT voice pack to CosyVoice Kaldi metadata."""

import argparse
from pathlib import Path


INSTRUCT = "You are a helpful assistant.<|endofprompt|>"


def write_split(directory: Path, pairs: list[tuple[Path, Path]], speaker: str) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    rows = []
    for wav, text_file in pairs:
        utterance = f"{speaker}_{wav.stem}"
        text = " ".join(text_file.read_text(encoding="utf-8-sig").split())
        if not text:
            raise ValueError(f"empty transcript: {text_file}")
        rows.append((utterance, wav.resolve(), text))

    (directory / "wav.scp").write_text(
        "".join(f"{utt} {wav}\n" for utt, wav, _ in rows), encoding="utf-8"
    )
    (directory / "text").write_text(
        "".join(f"{utt} {text}\n" for utt, _, text in rows), encoding="utf-8"
    )
    (directory / "utt2spk").write_text(
        "".join(f"{utt} {speaker}\n" for utt, _, _ in rows), encoding="utf-8"
    )
    (directory / "spk2utt").write_text(
        f"{speaker} {' '.join(utt for utt, _, _ in rows)}\n", encoding="utf-8"
    )
    (directory / "instruct").write_text(
        "".join(f"{utt} {INSTRUCT}\n" for utt, _, _ in rows), encoding="utf-8"
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path, help="directory containing matching .wav/.txt files")
    parser.add_argument("destination", type=Path, help="output directory")
    parser.add_argument("--speaker", default="cave")
    parser.add_argument("--dev-count", type=int, default=6)
    args = parser.parse_args()
    if args.dev_count < 2:
        raise SystemExit("--dev-count must be at least 2")

    wavs = sorted(args.source.glob("*.wav"))
    pairs = [(wav, wav.with_suffix(".txt")) for wav in wavs if wav.with_suffix(".txt").is_file()]
    if len(pairs) < args.dev_count + 2:
        raise SystemExit("not enough WAV/TXT pairs for train and dev splits")

    # Spread validation samples across the pack instead of taking one contiguous scene.
    dev_indexes = {
        round(index * (len(pairs) - 1) / (args.dev_count - 1))
        for index in range(args.dev_count)
    }
    train = [pair for index, pair in enumerate(pairs) if index not in dev_indexes]
    dev = [pair for index, pair in enumerate(pairs) if index in dev_indexes]
    write_split(args.destination / "train", train, args.speaker)
    write_split(args.destination / "dev", dev, args.speaker)
    print(f"prepared {len(train)} train and {len(dev)} dev utterances in {args.destination}")


if __name__ == "__main__":
    main()
