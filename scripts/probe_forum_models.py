#!/usr/bin/env python3
"""F01 synthetic-file feasibility probes; no audio device, downloads or production changes.

Use ``prepare`` before ``run``. GPU jobs run sequentially except the explicitly
labelled three-process contention stage. Cancellation targets only owned children.
The summary child is a measurement harness, not the packaged meeting worker.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
import unicodedata
import wave

ROOT = Path(__file__).resolve().parents[1]
CORPUS = ROOT / "fixtures/f01-model-probes/corpus.json"
DEFAULT_OUT = ROOT / "artifacts/local/f01-model-feasibility"


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def rows(path):
    if not path.exists():
        return []
    result = []
    for line in path.read_text(errors="replace").splitlines():
        try:
            result.append(json.loads(line))
        except json.JSONDecodeError:
            pass  # An active writer may not have flushed the final row yet.
    return result


def emit(stream, event, **values):
    stream.write(json.dumps({"event": event, "unix_ms": time.time_ns() / 1e6, **values}, ensure_ascii=False) + "\n")
    stream.flush()


def local_model(path, required):
    path = Path(path).expanduser().resolve(strict=True)
    for name in required:
        candidate = path / name
        if not candidate.is_file() or candidate.stat().st_size == 0:
            raise ValueError(f"Incomplete local model: {candidate}")
    return path


def prepare(args):
    corpus = json.loads(CORPUS.read_text())
    output = Path(args.output).resolve()
    audio_dir = output / "audio"
    audio_dir.mkdir(parents=True, exist_ok=True)
    rate = corpus["sample_rate"]
    cases, manifest = [], []
    for case in corpus["cases"]:
        frames = bytearray()
        segments = []
        for index, segment in enumerate(case["segments"]):
            base = audio_dir / f"{case['id']}-{index}"
            source = base.with_suffix(".txt")
            source.write_text(segment["text"])
            aiff, wav = base.with_suffix(".aiff"), base.with_suffix(".wav")
            subprocess.run(["say", "-v", corpus["voices"][segment["voice"]], "-r", "185", "-f", str(source), "-o", str(aiff)], check=True, timeout=60)
            subprocess.run(["afconvert", "-f", "WAVE", "-d", "LEI16@16000", "-c", "1", str(aiff), str(wav)], check=True, timeout=30)
            with wave.open(str(wav), "rb") as reader:
                assert (reader.getnchannels(), reader.getsampwidth(), reader.getframerate()) == (1, 2, rate)
                pcm = reader.readframes(reader.getnframes())
            start = len(frames) / 2 / rate
            frames.extend(pcm)
            segments.append({**segment, "voice_name": corpus["voices"][segment["voice"]], "start_seconds": start, "end_seconds": len(frames) / 2 / rate})
            frames.extend(b"\0\0" * round(rate * segment.get("gap_after_ms", 0) / 1000))
        if case.get("silence_ms"):
            frames.extend(b"\0\0" * round(rate * case["silence_ms"] / 1000))
        path = audio_dir / f"{case['id']}.wav"
        with wave.open(str(path), "wb") as writer:
            writer.setparams((1, 2, rate, 0, "NONE", "not compressed"))
            writer.writeframes(frames)
        manifest.append({**case, "wav": str(path), "duration_ms": len(frames) / 2 / rate * 1000, "segments": segments, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()})
        for hint in case["hints"]:
            cases.append({"id": f"{case['id']}::{hint or 'EMPTY'}", "wav": str(path), "language": hint})
    write_json(output / "audio-manifest.json", manifest)
    write_json(output / "asr-cases.json", cases)
    translations = [{"id": f"{case['id']}::{repeat}", "source_language": "en" if case['id'].startswith(('en_', 'short_')) else "mixed" if 'mixed' in case['id'] or 'alternating' in case['id'] else "zh", "target_language": "zh" if case['id'].startswith(('en_', 'short_')) else "en", "text": case["reference"]} for repeat in range(4) for case in corpus["cases"] if case["reference"]]
    write_json(output / "translation-cases.json", translations)
    prompt = "以下是虚构会议转录。只输出一个JSON对象，键为decisions、risks、actions和evidence；actions包含owner、task、due；evidence使用原始turn id。必须区分未批准事项和已决定事项，保留否定、数量和期限，不补造事实。\n" + json.dumps(corpus["summary_turns"], ensure_ascii=False)
    write_json(output / "summary-jobs.json", [{"id": f"summary-{i+1}", "prompt": prompt, "max_tokens": 384} for i in range(2)])
    print(f"prepared {len(manifest)} synthetic files, {len(cases)} Qwen hint cases, {len(translations)} translation cases", flush=True)


def summary_worker(args):
    import mlx.core as mx
    from mlx_lm import load, stream_generate
    from mlx_lm.sample_utils import make_sampler
    model_path = local_model(args.model, ["config.json", "tokenizer.json", "model.safetensors"])
    jobs = json.loads(Path(args.cases).read_text())
    with Path(args.result).open("w") as out:
        started = time.perf_counter()
        emit(out, "loading", pid=os.getpid(), model=str(model_path))
        model, tokenizer = load(str(model_path), lazy=False)
        mx.synchronize()
        load_ms = (time.perf_counter() - started) * 1000
        emit(out, "ready", load_ms=load_ms, versions={key: importlib.metadata.version(key) for key in ["mlx", "mlx-lm"]})
        admitted = time.perf_counter()
        for job in jobs:
            queue_ms = (time.perf_counter() - admitted) * 1000
            prompt = tokenizer.apply_chat_template([{"role": "user", "content": job["prompt"]}], tokenize=False, add_generation_prompt=True, enable_thinking=False)
            start = time.perf_counter()
            emit(out, "case_started", case_id=job["id"], queue_wait_ms=queue_ms, queue_scope="all probe jobs admitted at model ready; sequential Python generator")
            text, first_ms, response = "", None, None
            for response in stream_generate(model, tokenizer, prompt=prompt, max_tokens=job["max_tokens"], sampler=make_sampler(temp=0.0)):
                text += response.text
                if first_ms is None:
                    first_ms = (time.perf_counter() - start) * 1000
                    emit(out, "first_token", case_id=job["id"], first_token_ms=first_ms)
            mx.synchronize()
            try:
                parsed = json.loads(text)
                json_error = None
            except json.JSONDecodeError as exc:
                parsed, json_error = None, str(exc)
            emit(out, "case_result", case_id=job["id"], text=text, parsed=parsed, json_error=json_error, first_token_ms=first_ms, inference_ms=(time.perf_counter()-start)*1000, queue_wait_ms=queue_ms, load_ms=load_ms, prompt_tokens=response.prompt_tokens if response else None, generated_tokens=response.generation_tokens if response else None, finish_reason=response.finish_reason if response else None, mlx_peak_bytes=mx.get_peak_memory(), max_tokens=job["max_tokens"], scope="standalone measurement generator; no real meeting worker RPC or durable job queue")
        emit(out, "finished")


def whisper_worker(args):
    import mlx.core as mx
    import numpy as np
    import mlx_whisper
    from mlx_whisper.audio import log_mel_spectrogram, pad_or_trim, N_FRAMES, N_SAMPLES
    from mlx_whisper.transcribe import ModelHolder
    model_path = local_model(args.model, ["config.json", "weights.safetensors"])
    cases = json.loads(Path(args.cases).read_text())
    with Path(args.result).open("w") as out:
        start = time.perf_counter()
        model = ModelHolder.get_model(str(model_path), mx.float16)
        mx.synchronize()
        load_ms = (time.perf_counter()-start)*1000
        emit(out, "ready", load_ms=load_ms, model=str(model_path))
        for case in cases:
            with wave.open(case["wav"], "rb") as reader:
                audio = np.frombuffer(reader.readframes(reader.getnframes()), dtype=np.int16).astype(np.float32) / 32768
            start = time.perf_counter()
            mel = log_mel_spectrogram(audio, n_mels=model.dims.n_mels, padding=N_SAMPLES)
            _, probs = model.detect_language(pad_or_trim(mel, N_FRAMES, axis=-2).astype(mx.float16))
            diagnostic_lid_ms = (time.perf_counter()-start)*1000
            top = sorted(probs.items(), key=lambda pair:pair[1], reverse=True)[:3]
            start = time.perf_counter()
            result = mlx_whisper.transcribe(audio, path_or_hf_repo=str(model_path), language=None, task="transcribe", temperature=0.0, condition_on_previous_text=False, verbose=None)
            emit(out, "case_result", case_id=case["id"], configured_language=None, detected_language=result["language"], text=result["text"], language_top3=top, diagnostic_lid_ms=diagnostic_lid_ms, inference_ms=(time.perf_counter()-start)*1000, load_ms=load_ms, audio_ms=case["duration_ms"], detection_scope="one dominant language from up to first 30 seconds, not mixed-language spans", segments=result["segments"])
        emit(out, "finished")


def model_snapshot(roots):
    return {str(path): [path.stat().st_size, path.stat().st_mtime_ns] for root in roots for path in Path(root).rglob("*") if path.is_file()}


def launch(name, command, directory, env):
    directory.mkdir(parents=True, exist_ok=True)
    out, err = (directory / f"{name}.stdout.log").open("w"), (directory / f"{name}.stderr.log").open("w")
    start = time.perf_counter()
    process = subprocess.Popen(command, stdout=out, stderr=err, env=env, cwd=ROOT, start_new_session=True)
    return {"name": name, "command": command, "process": process, "files": [out, err], "started": start, "pid": process.pid, "started_unix_ms": time.time_ns()/1e6, "rss_peak_kib": 0, "rss_samples": 0}


def sample(jobs):
    alive = []
    for job in jobs:
        if job["process"].poll() is None:
            alive.append(job)
        elif "finished_monotonic" not in job:
            job["finished_monotonic"] = time.perf_counter()
    total = 0
    if alive:
        output = subprocess.run(["ps", "-o", "pid=,rss=", "-p", ",".join(str(job["pid"]) for job in alive)], capture_output=True, text=True).stdout
        memory = {int(line.split()[0]): int(line.split()[1]) for line in output.splitlines() if len(line.split()) == 2}
        for job in alive:
            rss = memory.get(job["pid"], 0)
            job["rss_peak_kib"] = max(job["rss_peak_kib"], rss)
            job["rss_samples"] += 1
            total += rss
    return total


def finish(job):
    code = job["process"].wait(timeout=5)
    for handle in job["files"]:
        handle.close()
    return {key: value for key, value in {**job, "returncode": code, "wall_ms": (job.get("finished_monotonic", time.perf_counter())-job["started"])*1000, "process_exited": job["process"].poll() is not None}.items() if key not in {"process", "files", "started", "finished_monotonic"}}


def await_jobs(jobs, timeout=240):
    began, peak = time.perf_counter(), 0
    try:
        while any(job["process"].poll() is None for job in jobs):
            peak = max(peak, sample(jobs))
            if time.perf_counter()-began > timeout:
                raise TimeoutError("probe group exceeded its deadline")
            time.sleep(0.1)
        return {"simultaneously_sampled_rss_peak_kib": peak, "jobs": [finish(job) for job in jobs]}
    finally:
        for job in jobs:
            if job["process"].poll() is None:
                job["process"].kill()
                job["process"].wait()


def normalized(text):
    return ''.join(ch for ch in unicodedata.normalize("NFKC", text).casefold() if ch.isalnum())


def edit_distance(a, b):
    previous = list(range(len(b)+1))
    for i, left in enumerate(a, 1):
        current = [i]
        for j, right in enumerate(b, 1):
            current.append(min(current[-1]+1, previous[j]+1, previous[j-1]+(left != right)))
        previous = current
    return previous[-1]


def run(args):
    output = Path(args.output).resolve()
    manifest = json.loads((output / "audio-manifest.json").read_text())
    roots = [local_model(args.asr_model, ["config.json", "tokenizer.json", "model.safetensors"]), local_model(args.translator_model, ["config.json", "tokenizer.json", "model.safetensors"]), local_model(args.summary_model, ["config.json", "tokenizer.json", "model.safetensors"]), local_model(args.whisper_model, ["config.json", "weights.safetensors"])]
    before = model_snapshot(roots)
    env = {**os.environ, "HF_HUB_OFFLINE":"1", "TRANSFORMERS_OFFLINE":"1", "HF_DATASETS_OFFLINE":"1", "TOKENIZERS_PARALLELISM":"false"}
    script, python = str(Path(__file__).resolve()), str(Path(args.python).absolute())
    asr_bin, translator_bin = str(Path(args.asr_bin).resolve()), str(Path(args.translator_bin).resolve())
    report = {"scope":"synthetic direct-worker feasibility, not live capture, production scheduler, 3-second or 90-minute acceptance", "python":python, "model_paths":[str(root) for root in roots], "rss_measurement":"macOS ps resident KiB every approximately 100 ms for direct child PIDs; does not measure all GPU/system memory", "stages":{}}
    def command(role, folder, cases=None):
        result = folder / f"{role}.jsonl"
        if role == 'asr':
            return [asr_bin, str(roots[0]), str(cases or output/'asr-cases.json'), str(result)]
        if role == 'translator':
            return [translator_bin, str(roots[1]), str(cases or output/'translation-cases.json'), str(result)]
        if role == 'summary':
            return [python, script, 'summary-worker', '--model', str(roots[2]), '--cases', str(cases or output/'summary-jobs.json'), '--result', str(result)]
        return [python, script, 'whisper-worker', '--model', str(roots[3]), '--cases', str(output/'audio-manifest.json'), '--result', str(result)]
    def save():
        write_json(output/'report.json', report)
    for role in ['asr', 'translator', 'summary', 'whisper']:
        folder = output/'baseline'/role
        report['stages'][f'baseline_{role}'] = await_jobs([launch(role, command(role, folder), folder, env)])
        save()
        print(f"completed baseline {role}: {report['stages'][f'baseline_{role}']['jobs'][0]['returncode']}", flush=True)
        if report['stages'][f'baseline_{role}']['jobs'][0]['returncode'] != 0:
            raise RuntimeError(f"baseline {role} failed; inspect local logs")
    folder = output/'concurrent'
    report['stages']['concurrent'] = await_jobs([launch(role, command(role, folder), folder, env) for role in ['asr','translator','summary']])
    save()
    print('completed explicit three-process contention stage', flush=True)
    # Termination is a process-boundary probe, not a claim of cooperative job cancellation.
    for role in ['asr', 'translator', 'summary']:
        folder = output/'lifecycle'/role
        folder.mkdir(parents=True, exist_ok=True)
        source = output/({'asr':'asr-cases.json','translator':'translation-cases.json','summary':'summary-jobs.json'}[role])
        cases = json.loads(source.read_text())
        long_cases = cases * 8
        write_json(folder/'many.json', long_cases if role != 'translator' else [{**case,'id':f"{case['id']}::cancel-{i}"} for i,case in enumerate(long_cases)])
        job = launch(role, command(role, folder, folder/'many.json'), folder, env)
        deadline = time.perf_counter()+90
        trigger = None
        while job['process'].poll() is None and time.perf_counter() < deadline:
            sample([job])
            events = rows(folder/f'{role}.jsonl')
            trigger = next((event for event in events if event.get('event') == ('case_started' if role=='asr' else 'first_token') or role=='translator' and event.get('case_id')), None)
            if trigger:
                break
            time.sleep(0.05)
        if not trigger or job['process'].poll() is not None:
            if job['process'].poll() is None:
                job['process'].kill()
            result = finish(job)
            raise RuntimeError(f'No active cancellation point for {role}: {result}')
        time.sleep(0.05)
        cancelled = time.perf_counter()
        job['process'].send_signal(signal.SIGTERM)
        escalated = False
        try:
            job['process'].wait(timeout=2)
        except subprocess.TimeoutExpired:
            escalated = True
            job['process'].kill()
            job['process'].wait(timeout=3)
        cancel_ms = (time.perf_counter()-cancelled)*1000
        result = finish(job)
        restart = folder/'restart'
        restart.mkdir(parents=True, exist_ok=True)
        write_json(restart/'one.json', cases[:1])
        restarted = await_jobs([launch(role, command(role, restart, restart/'one.json'), restart, env)])
        report['stages'][f'lifecycle_{role}'] = {"trigger":trigger, "trigger_scope":"ASR before inference; summary after first token; translation after first completion with additional queued cases", "cancel_to_exit_ms":cancel_ms,"sigkill_escalated":escalated,"cancelled_process":result,"restart":restarted}
        save()
        print(f'completed cancel/restart {role}', flush=True)
    refs = {case['id']:case['reference'] for case in manifest}
    quality=[]
    for role in ['asr','whisper']:
        for row in rows(output/'baseline'/role/f'{role}.jsonl'):
            if row.get('event') != 'case_result':
                continue
            base_id=row['case_id'].split('::')[0]
            expected, actual=normalized(refs[base_id]), normalized(row['text'])
            quality.append({**row,'reference':refs[base_id],'engine':role,'normalized_character_edit_ratio':edit_distance(expected,actual)/len(expected) if expected else None,'silence_nonempty': bool(actual) if not expected else None,'metric_warning':'literal synthetic script comparison; numbers, pronunciation and multilingual spelling are not normalized semantically'})
    write_json(output/'quality.json',quality)
    report['model_directory_metadata_unchanged']=before==model_snapshot(roots)
    report['model_snapshot_scope']='file list, sizes and mtime_ns before/after; no inference writes permitted by harness'
    report['completed_unix_ms']=time.time_ns()/1e6
    save()
    print('all probe stages complete; see report.json and quality.json',flush=True)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    sub=parser.add_subparsers(dest='mode',required=True)
    prepare_parser=sub.add_parser('prepare')
    prepare_parser.add_argument('--output',default=str(DEFAULT_OUT))
    run_parser=sub.add_parser('run')
    run_parser.add_argument('--output',default=str(DEFAULT_OUT))
    run_parser.add_argument('--python',default=str(ROOT/'.venv/bin/python'))
    run_parser.add_argument('--asr-bin',default='/tmp/aivf-cargo-target/debug/examples/forum_asr_language_probe')
    run_parser.add_argument('--translator-bin',default='/tmp/aivf-cargo-target/debug/examples/forum_translation_probe')
    home=Path.home()
    run_parser.add_argument('--asr-model',default=str(home/'.OminiX/models/qwen3-asr-1.7b'))
    run_parser.add_argument('--translator-model',default=str(home/'.OminiX/models/Qwen3.5-2B-MLX-4bit'))
    run_parser.add_argument('--summary-model',required=True)
    run_parser.add_argument('--whisper-model',required=True)
    for mode in ['summary-worker','whisper-worker']:
        child=sub.add_parser(mode)
        for arg in ['model','cases','result']:
            child.add_argument('--'+arg,required=True)
    args=parser.parse_args()
    {'prepare':prepare,'run':run,'summary-worker':summary_worker,'whisper-worker':whisper_worker}[args.mode](args)


if __name__=='__main__':
    main()
