#!/usr/bin/env python3
import argparse
import hashlib
import logging
import os
import threading

os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")
os.environ.setdefault("COQUI_TOS_AGREED", "1")

import numpy as np
import soundfile as sf
import torch
import torchaudio
import uvicorn
from fastapi import FastAPI, HTTPException
from fastapi.responses import StreamingResponse
from pydantic import BaseModel, Field
from TTS.api import TTS
from TTS.tts.models import xtts as xtts_module


app = FastAPI(title="KOMP XTTS v2", docs_url=None, redoc_url=None)
tts_api = None
tts_model = None
device_name = "cpu"
generation = 0
generation_lock = threading.Lock()
conditioning_lock = threading.Lock()
TARGET_SAMPLE_RATE = 16000


def load_audio_without_torchcodec(path, sample_rate):
    """Load KOMP's normalized WAV without TorchCodec or FFmpeg."""
    audio, source_rate = sf.read(path, dtype="float32", always_2d=True)
    speech = torch.from_numpy(audio.T.copy())
    if speech.shape[0] > 1:
        speech = speech.mean(dim=0, keepdim=True)
    if source_rate != sample_rate:
        speech = torchaudio.functional.resample(speech, source_rate, sample_rate)
    return speech.clamp(-1.0, 1.0)


xtts_module.load_audio = load_audio_without_torchcodec


class SynthesisRequest(BaseModel):
    text: str = Field(min_length=1)
    voice_id: str
    prompt_wav: str
    conditioning_path: str
    language: str = "ru"
    speed: float = Field(default=1.0, ge=0.5, le=2.0)
    stream: bool = True


def sample_fingerprint(path):
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def load_conditioning(request):
    fingerprint = sample_fingerprint(request.prompt_wav)
    with conditioning_lock:
        if os.path.isfile(request.conditioning_path):
            saved = torch.load(request.conditioning_path, map_location=device_name, weights_only=False)
            if saved.get("fingerprint") == fingerprint:
                return saved["gpt_cond_latent"].to(device_name), saved["speaker_embedding"].to(device_name)

        gpt_cond_latent, speaker_embedding = tts_model.get_conditioning_latents(
            audio_path=[request.prompt_wav]
        )
        os.makedirs(os.path.dirname(request.conditioning_path), exist_ok=True)
        torch.save(
            {
                "fingerprint": fingerprint,
                "gpt_cond_latent": gpt_cond_latent.detach().cpu(),
                "speaker_embedding": speaker_embedding.detach().cpu(),
            },
            request.conditioning_path,
        )
        return gpt_cond_latent.to(device_name), speaker_embedding.to(device_name)


@app.get("/health")
def health():
    return {
        "status": "ok" if tts_model is not None else "loading",
        "model_loaded": tts_model is not None,
        "device": device_name,
        "provider": "xtts",
    }


@app.post("/v1/cancel")
def cancel():
    global generation
    with generation_lock:
        generation += 1
    return {"ok": True}


@app.post("/v1/synthesize")
def synthesize(request: SynthesisRequest):
    if tts_model is None:
        raise HTTPException(status_code=503, detail="model is not loaded")
    if not os.path.isfile(request.prompt_wav):
        raise HTTPException(status_code=400, detail="voice prompt file does not exist")

    gpt_cond_latent, speaker_embedding = load_conditioning(request)
    with generation_lock:
        request_generation = generation

    def audio_chunks():
        chunks = tts_model.inference_stream(
            request.text,
            request.language,
            gpt_cond_latent,
            speaker_embedding,
            speed=request.speed,
            enable_text_splitting=True,
        )
        for speech in chunks:
            with generation_lock:
                if request_generation != generation:
                    break
            speech = speech.detach().float().cpu().reshape(1, -1)
            if tts_model.config.audio.output_sample_rate != TARGET_SAMPLE_RATE:
                speech = torchaudio.functional.resample(
                    speech,
                    tts_model.config.audio.output_sample_rate,
                    TARGET_SAMPLE_RATE,
                )
            pcm = (speech.squeeze(0).numpy().clip(-1.0, 1.0) * 32767.0).astype(np.int16)
            yield pcm.tobytes()

    return StreamingResponse(
        audio_chunks(),
        media_type="application/octet-stream",
        headers={
            "X-Sample-Rate": str(TARGET_SAMPLE_RATE),
            "X-Channels": "1",
            "X-Sample-Format": "s16le",
        },
    )


def choose_device(requested):
    if requested == "cuda":
        if not torch.cuda.is_available():
            raise RuntimeError("CUDA was requested but is unavailable")
        return "cuda"
    if requested == "mps":
        if not torch.backends.mps.is_available():
            raise RuntimeError("MPS was requested but is unavailable")
        return "mps"
    if requested == "cpu":
        return "cpu"
    if torch.cuda.is_available():
        return "cuda"
    if torch.backends.mps.is_available():
        try:
            torch.zeros(1, device="mps")
            return "mps"
        except Exception:
            logging.warning("MPS probe failed; using CPU", exc_info=True)
    return "cpu"


def main():
    global tts_api, tts_model, device_name
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", default="tts_models/multilingual/multi-dataset/xtts_v2")
    parser.add_argument("--device", default="auto", choices=["auto", "cpu", "cuda", "mps"])
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=50010)
    args = parser.parse_args()

    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s xtts: %(message)s")
    device_name = choose_device(args.device)
    logging.info("loading %s on %s", args.model, device_name)
    try:
        tts_api = TTS(model_name=args.model, progress_bar=False).to(device_name)
    except Exception:
        if device_name != "mps":
            raise
        logging.warning("XTTS failed on MPS; retrying on CPU", exc_info=True)
        device_name = "cpu"
        tts_api = TTS(model_name=args.model, progress_bar=False).to(device_name)
    tts_model = tts_api.synthesizer.tts_model
    logging.info("model loaded; starting local API")
    uvicorn.run(app, host=args.host, port=args.port, log_level="info")


if __name__ == "__main__":
    main()
