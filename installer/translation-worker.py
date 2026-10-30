"""Kadr local translation worker. JSON lines on stdin/stdout; no network access."""
import json
import os
from pathlib import Path
import sys

# utf-8-sig: Windows PowerShell may put a BOM before the first line it pipes in.
sys.stdin.reconfigure(encoding="utf-8-sig")
sys.stdout.reconfigure(encoding="utf-8")
os.environ.setdefault("OMP_NUM_THREADS", "2")
ROOT = Path(__file__).resolve().parent
ENGINES = {}

def engine(source, target):
    key = (source, target)
    if key not in ENGINES:
        import ctranslate2
        import sentencepiece
        folders = sorted((ROOT / "models").glob(f"translate-{source}_{target}-*"))
        if not folders:
            raise ValueError("Языковая модель не установлена")
        folder = folders[-1]
        translator = ctranslate2.Translator(str(folder / "model"), device="cpu", compute_type="int8", inter_threads=1, intra_threads=2)
        tokenizer = sentencepiece.SentencePieceProcessor(model_file=str(folder / "sentencepiece.model"))
        ENGINES[key] = (translator, tokenizer)
    return ENGINES[key]

def translate(text, target):
    source = "ru" if target == "en" else "en"
    if target not in ("ru", "en"):
        raise ValueError("Офлайн доступны только английский и русский")
    translator, tokenizer = engine(source, target)
    lines = text.split("\n")
    result = []
    for line in lines:
        if not line.strip():
            result.append(line)
            continue
        tokens = tokenizer.encode(line, out_type=str)
        # Bounded chunks: no silent truncation by the inference engine.
        chunks = [tokens[i:i+192] for i in range(0, len(tokens), 192)]
        output = translator.translate_batch(chunks, beam_size=3, max_batch_size=4, max_input_length=0, max_decoding_length=512)
        result.append(" ".join(tokenizer.decode(item.hypotheses[0]) for item in output))
    return {"text": "\n".join(result), "from": source, "to": target}

if __name__ == "__main__":
    for request in sys.stdin:
        try:
            request = request.lstrip("﻿").strip()
            if not request:
                continue
            payload = json.loads(request)
            reply = translate(payload["text"], payload["to"])
        except Exception as error:
            reply = {"error": str(error)}
        print(json.dumps(reply, ensure_ascii=False), flush=True)
