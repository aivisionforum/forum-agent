"""Score one pipeline run against a fixture's ground truth.

    .venv/bin/python -m stress.metrics <condition>

Reads data/<room>_transcript.jsonl and data/<room>_translations.jsonl (room
== condition name) plus data/stress/<condition>_ref.json, and reports the
numbers that decide whether subtitles are usable in a room:

  cer_zh / wer_en        ASR accuracy per language
  coverage               reference utterances that produced a segment
  halluc_chars_per_min   text emitted where nobody was speaking
  diar_purity            per-true-speaker label consistency (duration-weighted)
  label_churn            distinct predicted labels per true speaker
  frag_rate              segments not ending on sentence-final punctuation
  forced_closes          segments hitting MAX_SEGMENT_SECONDS
  lang_flips             lang detected such that translation goes the wrong way
  translation_drop       segments with missing or empty translation
"""
import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

_CJK = re.compile(r"[一-鿿]")
_SENT_END = tuple("。？！…!?.")
_PUNCT = re.compile(r"[\s，。！？、；：,.!?;:'\"“”‘’()（）\-—]+")


def _norm_zh(s: str) -> str:
    return _PUNCT.sub("", s)


def _norm_en(s: str) -> list:
    return _PUNCT.sub(" ", s.lower()).split()


def _lev(a, b) -> int:
    """Levenshtein distance over sequences (chars for zh, words for en)."""
    if not a:
        return len(b)
    prev = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        cur = [i]
        for j, y in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1,
                           prev[j - 1] + (x != y)))
        prev = cur
    return prev[-1]


def _overlap(a0, a1, b0, b1) -> float:
    return max(0.0, min(a1, b1) - max(a0, b0))


def ref_lang(text: str) -> str:
    return "zh" if _CJK.search(text) else "en"


def score(condition: str, room: str | None = None) -> dict:
    room = room or condition
    ref = json.loads(Path(f"data/stress/{condition}_ref.json").read_text())
    tpath = Path(f"data/{room}_transcript.jsonl")
    segs = [json.loads(l) for l in tpath.read_text().splitlines()] \
        if tpath.exists() else []
    xpath = Path(f"data/{room}_translations.jsonl")
    trans = {}
    if xpath.exists():
        for i, l in enumerate(xpath.read_text().splitlines(), 1):
            e = json.loads(l)
            trans[e.get("id", i)] = e.get("translation", "")

    out = {"condition": condition, "segments": len(segs),
           "ref_utterances": len(ref)}
    # Rate denominators must be the AUDIO duration: deriving it from the
    # segments makes one short false positive in a silent file look like a
    # torrent (1 junk char in a 1.2 s segment reads as 48 chars/min).
    try:
        import soundfile as sf
        dur_min = sf.info(f"data/stress/{condition}.wav").duration / 60.0
    except Exception:
        dur_min = (max([u["t_end"] for u in ref], default=0.0) or
                   max([s["t_end"] for s in segs], default=60.0)) / 60.0

    # --- hallucination: text where the reference has no speech at all ------
    halluc_chars = 0
    for s in segs:
        spoken = sum(_overlap(s["t_start"], s["t_end"], u["t_start"], u["t_end"])
                     for u in ref)
        span = max(1e-6, s["t_end"] - s["t_start"])
        if spoken / span < 0.2:
            halluc_chars += len(_norm_zh(s.get("text", "")))
    out["halluc_chars_per_min"] = round(halluc_chars / max(dur_min, 1e-6), 1)
    out["halluc_segments"] = sum(
        1 for s in segs
        if sum(_overlap(s["t_start"], s["t_end"], u["t_start"], u["t_end"])
               for u in ref) / max(1e-6, s["t_end"] - s["t_start"]) < 0.2)

    if not ref:  # silence soak: hallucination is the whole story
        out["note"] = "no reference speech; any text is a false positive"
        return out

    # --- pair each reference utterance with its best-overlapping segment ---
    pairs = []
    for u in ref:
        best, best_ov = None, 0.0
        for s in segs:
            ov = _overlap(u["t_start"], u["t_end"], s["t_start"], s["t_end"])
            if ov > best_ov:
                best, best_ov = s, ov
        need = 0.5 * (u["t_end"] - u["t_start"])
        pairs.append((u, best if best_ov > need else None))
    covered = [(u, s) for u, s in pairs if s is not None]
    out["coverage"] = round(len(covered) / len(ref), 3)

    # --- ASR error rates, per language ------------------------------------
    zh_err = zh_len = en_err = en_len = 0
    flips = 0
    for u, s in covered:
        rl = ref_lang(u["text"])
        if rl == "zh":
            a, b = _norm_zh(u["text"]), _norm_zh(s.get("text", ""))
            zh_err += _lev(a, b); zh_len += len(a)
            if s.get("lang") == "en":       # zh spoken, tagged en
                flips += 1                  # -> translated INTO Chinese
        else:
            a, b = _norm_en(u["text"]), _norm_en(s.get("text", ""))
            en_err += _lev(a, b); en_len += len(a)
            if s.get("lang") in ("zh", "mixed"):
                flips += 1                  # -> translated INTO English
    out["cer_zh"] = round(zh_err / zh_len, 3) if zh_len else None
    out["wer_en"] = round(en_err / en_len, 3) if en_len else None
    out["lang_flips"] = flips

    # --- content accuracy, aligned the other way --------------------------
    # When turns never close, one 12 s segment spans several reference
    # utterances, and pairing 1 ref <-> 1 segment charges the whole
    # difference to ASR error. Scoring each SEGMENT against the joined text
    # of every utterance it covers separates "wrong words" from "wrong
    # segment boundaries" -- both matter, for different reasons.
    zh_e = zh_l = en_e = en_l = 0
    for sg in segs:
        inside = [u for u in ref
                  if _overlap(sg["t_start"], sg["t_end"],
                              u["t_start"], u["t_end"])
                  > 0.3 * (u["t_end"] - u["t_start"])]
        if not inside:
            continue
        joined = "".join(u["text"] for u in inside)
        if ref_lang(joined) == "zh":
            a, b = _norm_zh(joined), _norm_zh(sg.get("text", ""))
            zh_e += _lev(a, b); zh_l += len(a)
        else:
            a, b = _norm_en(joined), _norm_en(sg.get("text", ""))
            en_e += _lev(a, b); en_l += len(a)
    out["content_cer_zh"] = round(zh_e / zh_l, 3) if zh_l else None
    out["content_wer_en"] = round(en_e / en_l, 3) if en_l else None
    out["utt_per_segment"] = round(len(ref) / len(segs), 2) if segs else None

    # --- diarization: purity and churn ------------------------------------
    by_true = defaultdict(Counter)
    for u, s in covered:
        by_true[u["speaker"]][s.get("speaker_id", "?")] += (
            u["t_end"] - u["t_start"])
    purities, churn = [], []
    for true_spk, counts in by_true.items():
        total = sum(counts.values())
        purities.append(max(counts.values()) / total)
        churn.append(len(counts))
    out["true_speakers"] = len({u["speaker"] for u in ref})
    out["pred_speakers"] = len({s.get("speaker_id") for s in segs})
    out["diar_purity"] = round(sum(purities) / len(purities), 3) if purities else None
    out["label_churn"] = round(sum(churn) / len(churn), 2) if churn else None
    # Purity cannot see MERGING: if two people always get the same label,
    # each of them is perfectly "pure". Count the collision directly --
    # distinct true speakers sharing one predicted label. MAX_SPEAKERS = 8
    # guarantees this above 8 voices, and the centroid update in diarize.py
    # never splits a label back apart.
    by_pred = defaultdict(set)
    for u, sg in covered:
        by_pred[sg.get("speaker_id", "?")].add(u["speaker"])
    if by_pred:
        collisions = [len(v) for v in by_pred.values()]
        out["merge_mean"] = round(sum(collisions) / len(collisions), 2)
        out["merge_max"] = max(collisions)
        out["labels_used"] = len(by_pred)  # < true_speakers means merging

    # --- segmentation shape ------------------------------------------------
    if segs:
        durs = [s["t_end"] - s["t_start"] for s in segs]
        out["seg_seconds_mean"] = round(sum(durs) / len(durs), 2)
        out["forced_closes"] = sum(1 for d in durs if d >= 11.5)
        out["frag_rate"] = round(
            sum(1 for s in segs
                if not s.get("text", "").rstrip().endswith(_SENT_END))
            / len(segs), 3)

    # --- translation -------------------------------------------------------
    if segs:
        missing = sum(1 for i in range(1, len(segs) + 1)
                      if not trans.get(i, "").strip())
        out["translation_drop"] = round(missing / len(segs), 3)
    return out


def main() -> None:
    conds = sys.argv[1:] or [p.stem[:-4] for p in
                             Path("data/stress").glob("*_ref.json")]
    rows = [score(c) for c in conds]
    print(json.dumps(rows, ensure_ascii=False, indent=1))


if __name__ == "__main__":
    main()
