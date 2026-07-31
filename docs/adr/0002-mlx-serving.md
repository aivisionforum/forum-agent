# ADR 0002: All LLM serving moves from Ollama to a managed mlx-lm server

Date: 2026-07-31. Status: accepted. Supersedes the serving half of ADR 0001
(model choices there remain: Qwen3-8B class live, Qwen2.5-32B class batch).

## Context
Live use showed insight generation and translation want more headroom, and
venue operation must be zero-touch: no model management on site. Ollama
(llama.cpp/Metal) was the spec's default; ASR already runs on MLX.

## Decision
Serve all LLM calls from `mlx_lm.server` (OpenAI-compatible), launched and
owned by forum-agent itself (`llm.launch_server()`, one-command startup,
terminated atexit). Started without `--model`: each request names its model,
so the translation/insight model (`mlx-community/Qwen3-8B-4bit`) and the
report model (`mlx-community/Qwen2.5-32B-Instruct-4bit`) coexist behind one
port. A single client (`forum_agent/llm.py`) is the only code path;
Qwen3 thinking is disabled per-call and `<think>` blocks are stripped.

## Why
- Benchmarked on the M3 Max: 46 vs 25 tok/s generation (Qwen3-8B, MLX vs
  Ollama) — ~1.8x; warm translation latency equal (~0.5 s) with slightly
  better completion quality (no untranslated fragments).
- Full M1 acceptance re-run on MLX: max lag 2.62 s / mean 0.38 s
  (Ollama baseline: 2.68 / 0.44). All 8 checks pass.
- One inference framework (MLX) for ASR + LLM; one child process to manage;
  Ollama no longer needs to be installed at all.

## Consequences
- Models download from Hugging Face on first use: the machine must run each
  path once (translation, insights, minutes, report) with internet BEFORE
  the event; after that everything is offline from the local HF cache.
- GPU contention between Whisper and the LLM remains physics; the 8B-live /
  32B-batch split from ADR 0001 still applies.
