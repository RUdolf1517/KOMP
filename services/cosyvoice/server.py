#!/usr/bin/env python3
import argparse
import logging
import os
import sys

os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")

import numpy as np
import torch
import torchaudio
import uvicorn
from fastapi import FastAPI, HTTPException
from fastapi.responses import StreamingResponse
from pydantic import BaseModel, Field


ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
SOURCE = os.path.join(ROOT, "vendor", "cosyvoice", "source")
sys.path.insert(0, SOURCE)
sys.path.insert(0, os.path.join(SOURCE, "third_party", "Matcha-TTS"))

from cosyvoice.cli.cosyvoice import AutoModel  # noqa: E402


app = FastAPI(title="KOMP CosyVoice", docs_url=None, redoc_url=None)
model = None


class SynthesisRequest(BaseModel):
    text: str = Field(min_length=1)
    voice_id: str
    prompt_text: str = Field(min_length=1)
    prompt_wav: str
    speed: float = Field(default=1.0, ge=0.5, le=2.0)
    stream: bool = True


@app.get("/health")
def health():
    return {
        "status": "ok" if model is not None else "loading",
        "model_loaded": model is not None,
        "device": "cuda" if torch.cuda.is_available() else "cpu",
    }


@app.post("/v1/synthesize")
def synthesize(request: SynthesisRequest):
    if model is None:
        raise HTTPException(status_code=503, detail="model is not loaded")
    if not os.path.isfile(request.prompt_wav):
        raise HTTPException(status_code=400, detail="voice prompt file does not exist")

    prompt = request.prompt_text
    if "<|endofprompt|>" not in prompt:
        prompt = "You are a helpful assistant.<|endofprompt|>" + prompt
    def audio_chunks():
        output = model.inference_zero_shot(
            request.text,
            prompt,
            request.prompt_wav,
            stream=request.stream,
            speed=request.speed,
        )
        for item in output:
            speech = item["tts_speech"].detach().cpu()
            if model.sample_rate != 16000:
                speech = torchaudio.functional.resample(speech, model.sample_rate, 16000)
            pcm = (speech.squeeze(0).numpy().clip(-1.0, 1.0) * 32767.0).astype(np.int16)
            yield pcm.tobytes()

    return StreamingResponse(
        audio_chunks(),
        media_type="application/octet-stream",
        headers={"X-Sample-Rate": "16000", "X-Channels": "1", "X-Sample-Format": "s16le"},
    )


def main():
    global model
    parser = argparse.ArgumentParser()
    parser.add_argument("--model-dir", required=True)
    parser.add_argument("--device", default="auto", choices=["auto", "cpu", "cuda"])
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=50000)
    args = parser.parse_args()

    if args.device == "cpu" or sys.platform == "darwin":
        os.environ["CUDA_VISIBLE_DEVICES"] = "-1"
    elif args.device == "cuda" and not torch.cuda.is_available():
        raise RuntimeError("CUDA was requested but is unavailable")

    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s cosyvoice: %(message)s")
    logging.info("loading model from %s", args.model_dir)
    model = AutoModel(model_dir=args.model_dir)
    logging.info("model loaded; starting local API")
    uvicorn.run(app, host=args.host, port=args.port, log_level="info")


if __name__ == "__main__":
    main()
