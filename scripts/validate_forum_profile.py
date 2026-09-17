#!/usr/bin/env python3
"""Validate the F00 configuration contract; never run product acceptance.

Uses only Python's standard library. --self-test exercises invalid policies
in memory, without creating fixtures, contacting services, or loading models.
"""

from __future__ import annotations

import argparse
from copy import deepcopy
import json
from pathlib import Path
import sys
import unittest


ROOT = Path(__file__).resolve().parents[1]
FILES = (
    "profiles/ai-vision-forum/product.json",
    "profiles/ai-vision-forum/profile.json",
    "docs/engineering/acceptance-profile.json",
)
CAPABILITIES = {f"C{number}" for number in range(1, 11)}
PHASES = {f"F{number:02}" for number in range(13)}


class ConfigurationError(ValueError):
    """An invalid F00 baseline, not a failed runtime benchmark."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ConfigurationError(message)


def load_documents(root: Path = ROOT) -> tuple[dict, dict, dict]:
    def reject_constant(value: str) -> None:
        raise ConfigurationError(f"Non-finite JSON number is not allowed: {value}")

    documents = []
    for relative_path in FILES:
        document = json.loads(
            (root / relative_path).read_text(encoding="utf-8"),
            parse_constant=reject_constant,
        )
        require(isinstance(document, dict), f"{relative_path}: expected an object")
        documents.append(document)
    return tuple(documents)


def validate(product: dict, profile: dict, acceptance: dict) -> None:
    try:
        _validate(product, profile, acceptance)
    except (KeyError, TypeError, IndexError) as error:
        raise ConfigurationError(f"Missing or incorrectly typed configuration field: {error}") from error


def _validate(product: dict, profile: dict, acceptance: dict) -> None:
    for document in (product, profile, acceptance):
        require(type(document["schema_version"]) is int and document["schema_version"] == 1,
                "Unsupported configuration schema_version")
    require(product["configuration_kind"] == "build_identity", "product must be build_identity")
    require(profile["configuration_kind"] == "runtime_policy", "profile must be runtime_policy")
    require("build_identity" not in profile and "access" not in profile,
            "Runtime profile must not supply build identity or commercial access")
    require(product["product_id"] == profile["profile_id"] == acceptance["profile_id"] == "ai-vision-forum",
            "Cross-file product/profile identity mismatch")
    require(profile["profile_version"] == acceptance["profile_version"] == "v1", "Profile version mismatch")
    require(product["runtime_profile"] == FILES[1], "Unexpected runtime profile path")
    require(profile["acceptance_profile"] == FILES[2], "Unexpected acceptance profile path")

    identity = product["build_identity"]
    expected_identity = {
        "bundle_id": "org.aivisionforum.agent", "product_name": "AI Vision Forum",
        "binary_name": "forum-shell", "url_scheme": "aivisionforum",
        "data_directory_name": "AI Vision Forum", "keychain_service": "org.aivisionforum.agent",
    }
    for key, value in expected_identity.items():
        require(identity[key] == value, f"Independent Forum build identity required: {key}")
    require(product["platform"]["target_triple"] == "aarch64-apple-darwin", "F00 targets Apple Silicon macOS")
    require(product["platform"]["minimum_os_version"] == "14.0", "Minimum OS baseline mismatch")
    require(product["platform"]["minimum_memory_gib"] is None,
            "Do not invent an unmeasured release memory minimum")
    access = product["access"]
    require(access["mode"] == "local_event", "Forum must use local event access")
    for key in ("commercial_login", "license_required", "billing_enabled"):
        require(access[key] is False, f"Forum must not require commercial access: {key}")
    require(access["account_provider"] is None, "Forum has no account provider dependency")
    updater = product["distribution"]["updater"]
    require(updater["enabled"] is False and updater["endpoint"] is None and updater["public_key"] is None,
            "F00 updater must remain disabled until an independent release feed is verified")
    require(product["lifecycle"]["start_capture_on_launch"] is False and
            product["lifecycle"]["restart_capture_after_crash"] is False,
            "Capture must not start automatically")

    deployment = profile["deployment"]
    require(deployment["room_count"] == 2 and deployment["machines_per_room"] == 1 and
            deployment["max_active_capture_sessions_per_machine"] == 1,
            "Formal Forum deployment requires two rooms, one capture owner per machine")
    require(deployment["shared_database"] is False, "Room databases must remain independent")
    for key in ("recording_default", "recording_required_for_formal_C3",
                "speaker_diarization_required_for_formal_C3", "persist_stable_transcript_when_recording_disabled"):
        require(profile["audio"][key] is True, f"Required recording/transcript policy missing: {key}")
    require(profile["language"]["fixed_direction_fallback_is_formal_C1_pass"] is False,
            "Fixed-direction fallback is not automatic bilingual C1 acceptance")
    publication = profile["publication"]
    require(publication["public_captions"] == "strict_manual_review", "Public captions require strict manual review")
    require(publication["public_summaries"] == "manual_review_required", "Public summaries require manual review")
    for key in ("unreviewed_partials_on_public_displays", "automatic_name_masking_is_approval",
                "raw_audio_and_transcript_on_participant_gateway"):
        require(publication[key] is False, f"Unsafe publication policy: {key}")
    require(publication["facilitator_questions"] == "private_only", "Facilitator suggestions must stay private")
    require(publication["fast_caption_and_review_publication_latency_reported_separately"] is True,
            "Fast and reviewed publication latency must be reported separately")
    require(publication["anonymous_content_guarantee_status"] == "requires_formal_event_acceptance",
            "F00 cannot claim absolute anonymity was verified")
    require(profile["network"]["cloud_inference_enabled"] is False and
            profile["network"]["cloud_polish_enabled"] is False and
            profile["network"]["venue_internet_required_after_preparation"] is False,
            "Forum event operation must remain local after preparation")
    require(profile["model_roles"]["load_optional_batch_during_capture"] is False and
            profile["model_roles"]["download_models_during_capture"] is False,
            "Capture may not load the optional batch model or download missing models")
    require(profile["analysis"]["empty_approved_set_fallback_to_drafts"] is False,
            "An empty approved set cannot fall back to drafts")

    require(acceptance["purpose"] == "execution_baseline_not_test_results" and
            acceptance["product_acceptance_status"] == "not_evaluated",
            "F00 configuration validation is not product acceptance")
    metrics = acceptance["metrics"]
    spec = metrics["c1_original_spec_latency"]
    require(spec["operator"] == "lt" and spec["value"] == 3 and spec["unit"] == "seconds" and
            spec["statistic"] == "unspecified_in_original_spec" and
            spec["classification"] == "formal_spec_unchanged",
            "Original SPEC <3s must not be replaced by a percentile or weaker operator")
    candidate = metrics["final_translation_latency"]
    require(candidate["classification"] == "development_candidate_not_formal_spec" and
            candidate["statistic"] == "p95" and candidate["value"] == 3000,
            "p95 <=3s must remain a development reference")
    require(metrics["public_caption_latency"]["includes_human_review_wait"] is True,
            "Public display latency must include human review wait")
    minutes = metrics["minutes_complete_draft"]
    require(minutes["value"] == 180 and minutes["unit"] == "seconds" and minutes["session_minutes"] == 90 and
            minutes["includes_queue_wait"] is True and minutes["partial_or_queued_counts_as_success"] is False and
            minutes["classification"] == "development_candidate_pending_F01_calibration",
            "Minutes candidate must cover a complete 90-minute meeting within 180 seconds including queue wait")
    require(acceptance["measurement_protocol"]["missing_or_failed_outputs_excluded_silently"] is False,
            "Missing and failed outputs must be reported")

    requirements = acceptance["requirements"]
    require(len(requirements) == 10 and {item["id"] for item in requirements} == CAPABILITIES,
            "Exactly C1-C10 are required, without duplicates")
    hardware = acceptance["hardware_profiles"]
    for item in requirements:
        require(item["required_for_forum_release"] is True and item["acceptance_status"] == "not_evaluated",
                f"{item['id']}: must remain required and unmeasured at F00")
        for field in ("phases", "hardware", "metrics", "test_methods", "failure_conditions", "evidence"):
            require(isinstance(item[field], list) and len(item[field]) > 0 and
                    all(isinstance(value, str) and value.strip() for value in item[field]),
                    f"{item['id']}: missing nonempty {field}")
        require(set(item["phases"]) <= PHASES, f"{item['id']}: unknown implementation phase")
        require(set(item["hardware"]) <= set(hardware), f"{item['id']}: unknown hardware profile")
        require(set(item["metrics"]) <= set(metrics), f"{item['id']}: unknown metric")
    gates = {gate["id"] for gate in acceptance["cross_cutting_gates"]}
    decisions = acceptance["open_decisions"]
    require(len(decisions) == 7 and {decision["id"] for decision in decisions} == {f"D{i:02}" for i in range(1, 8)},
            "F00 must explicitly retain D01-D07 unresolved decisions")
    for decision in decisions:
        require(decision["status"] == "open" and decision["resolve_by"] in PHASES and
                bool(decision["owner"]) and bool(decision["required_evidence"]),
                f"{decision['id']}: missing owner, phase, evidence, or unresolved status")
        require(bool(decision["blocks_formal"]) and set(decision["blocks_formal"]) <= CAPABILITIES | gates,
                f"{decision['id']}: invalid formal acceptance blockers")
    for metric in metrics.values():
        if "decision" in metric:
            require(metric["decision"] in {decision["id"] for decision in decisions}, "Unknown metric decision")


class ContractTests(unittest.TestCase):
    def test_checked_in_baseline(self) -> None:
        validate(*load_documents())

    def test_rejects_misleading_or_conflicting_configurations(self) -> None:
        baseline = load_documents()
        mutations = (
            (0, ("build_identity", "bundle_id"), "com.henlocal.translator"),
            (0, ("access", "license_required"), True),
            (0, ("distribution", "updater", "endpoint"), "https://example.invalid/translator/latest.json"),
            (1, ("publication", "public_captions"), "automatic"),
            (1, ("publication", "unreviewed_partials_on_public_displays"), True),
            (1, ("deployment", "machines_per_room"), 2),
            (2, ("metrics", "c1_original_spec_latency", "operator"), "lte"),
            (2, ("metrics", "c1_original_spec_latency", "statistic"), "p95"),
            (2, ("metrics", "minutes_complete_draft", "includes_queue_wait"), False),
            (2, ("metrics", "public_caption_latency", "includes_human_review_wait"), False),
            (2, ("product_acceptance_status",), "passed"),
            (2, ("requirements",), baseline[2]["requirements"][:-1]),
            (2, ("requirements",), baseline[2]["requirements"][:-1] + [baseline[2]["requirements"][0]]),
            (2, ("open_decisions",), []),
        )
        for document_index, keys, replacement in mutations:
            with self.subTest(document=document_index, keys=keys):
                documents = deepcopy(baseline)
                target = documents[document_index]
                for key in keys[:-1]:
                    target = target[key]
                target[keys[-1]] = replacement
                with self.assertRaises(ConfigurationError):
                    validate(*documents)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true", help="run baseline and negative contract checks")
    args = parser.parse_args()
    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(ContractTests)
        return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1
    try:
        validate(*load_documents())
    except (ConfigurationError, OSError, json.JSONDecodeError) as error:
        print(f"F00 configuration invalid: {error}", file=sys.stderr)
        return 1
    print("F00 configuration valid: C1-C10 mapped; D01-D07 remain open.")
    print("Configuration checks only. Models, hardware, release and event acceptance: NOT EVALUATED.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
