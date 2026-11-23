"""Shared metric constants and typed internal schemas."""

from __future__ import annotations

from pathlib import Path
from typing import Literal, NotRequired, TypedDict

Tier = Literal["gold", "silver", "bronze", "edited"]
Kind = Literal["prose", "spoken"]
Pool = Literal["rhythm", "diction", "format"]

Origin = Literal["ai-first", "writer-first"]
ProvenanceOrigin = Literal["unaided", "writer-first", "ai-first"]
Confirmation = Literal["none", "origin", "promoted"]

RuleKind = Literal["cut", "add"]
RuleStatus = Literal["confirmed", "rejected", "retired", "hypothesis", "sparse"]
ConfidenceLevel = Literal["low", "medium", "high"]

BOTH_DIRECTIONS = (
    "medianSentenceLength",
    "contractionsPer1k",
    "iPer1k",
    "questionsPer1k",
    "openerShare",
    "medianParagraphLength",
)

# Metrics a manifest `overrides` entry may pin: the two-direction metrics
# plus em dashes, which has its own comparison.
OVERRIDABLE_METRICS = BOTH_DIRECTIONS + ("emDashesPer1k",)

METRIC_LABELS: dict[str, str] = {
    "medianSentenceLength": "median sentence length",
    "contractionsPer1k": "contractions per 1k",
    "iPer1k": "first-person I per 1k",
    "questionsPer1k": "questions per 1k",
    "openerShare": "And/But/So opener share",
    "medianParagraphLength": "median paragraph length",
    "emDashesPer1k": "em dashes per 1k",
}


class Manifest(TypedDict):
    root: Path
    weights: dict[Tier, float]
    goldVoiceMinimum: int
    failUnder: int
    updateCheck: bool
    overrides: dict[str, float]
    placeholder: str
    pieces: list[object]


class ProvenanceJson(TypedDict):
    origin: str | None
    confirmation: str
    model: str | None
    tool: str | None


class NoteSaveResult(TypedDict):
    path: Path
    created: bool
    appended: bool


class MigrationResult(TypedDict):
    root: Path
    gitignore: Path
    changed: bool
    legacy_removed: bool
    created: list[str]
    tracked: tuple[str, ...]


class Distribution(TypedDict):
    short: float | None
    medium: float | None
    long: float | None


OpenerRates = TypedDict(
    "OpenerRates",
    {
        "and": float | None,
        "but": float | None,
        "so": float | None,
    },
)


class Metrics(TypedDict):
    rhythmWords: float
    rhythmSentences: float
    dictionWords: float
    formatParagraphs: float
    medianSentenceLength: float | int | None
    sentenceLengthDistribution: Distribution
    contractionsPer1k: float | None
    iPer1k: float | None
    emDashesPer1k: float | None
    questionsPer1k: float | None
    medianParagraphLength: float | int | None
    openerShare: float | None
    openers: OpenerRates


class SignaturePhrase(TypedDict):
    phrase: str
    count: int
    weight: float


class MetricBlock(TypedDict):
    metrics: Metrics
    signaturePhrases: list[SignaturePhrase]
    banList: list[str]
    tellsInUse: list[str]
    dictionEmpty: bool


class SectionBlock(TypedDict):
    pieces: int
    weighted: MetricBlock
    unweighted: MetricBlock


class TierCounts(TypedDict):
    gold: int
    silver: int
    bronze: int
    edited: int


class PieceCounts(TypedDict):
    prose: TierCounts
    spoken: TierCounts
    learned: int
    total: int


ProvenanceCounts = TypedDict(
    "ProvenanceCounts",
    {
        "unaided": int,
        "writer-first": int,
        "ai-first": int,
        "unknown": int,
    },
)


class EditedPoolInfo(TypedDict):
    pieces: int
    configuredWeight: float
    countedWeight: float
    maxShare: float


class FileRow(TypedDict):
    path: str
    tier: Tier
    kind: Kind
    section: str | None
    weight: float
    words: int
    learned: bool
    origin: str | None
    confirmation: str | None
    model: str | None
    tool: str | None


class ConfidenceMinimums(TypedDict):
    words: int
    pieces: int


class CorpusConfidence(TypedDict):
    level: ConfidenceLevel
    trustworthy: bool
    pieces: int
    words: int
    writtenWords: int
    sentences: int
    minimums: ConfidenceMinimums
    note: str


class WordCount(TypedDict):
    word: str
    count: int


MovedSpan = TypedDict(
    "MovedSpan",
    {
        "tokens": int,
        "text": str,
        "from": int,
        "to": int,
    },
)


class TokenAlignment(TypedDict):
    deletions: int
    insertions: int
    replacements: int
    movedTokens: int
    movedSpans: list[MovedSpan]
    unchangedTokens: int


class DiffSummary(TypedDict):
    wordsBefore: int
    wordsAfter: int
    wordsRemoved: int
    wordsAdded: int
    medianSentenceLengthBefore: float | int | None
    medianSentenceLengthAfter: float | int | None
    contractionsPer1kBefore: float | None
    contractionsPer1kAfter: float | None
    removedWords: list[WordCount]
    addedWords: list[WordCount]
    tokenEdits: int
    unchangedTokens: int
    deletions: int
    insertions: int
    replacements: int
    movedTokens: int
    movedSpans: list[MovedSpan]
    editRate: float
    editRateDenominator: int
    paragraphsBefore: int
    paragraphsAfter: int
    sentencesBefore: int
    sentencesAfter: int
    structuralChanges: list[dict[str, object]]
    structuralChangeCount: int


class EditLearnEntry(DiffSummary):
    slug: str
    date: object
    section: object
    origin: ProvenanceOrigin | None
    confirmation: Confirmation
    model: str | None
    tool: str | None
    tier: Tier


class EditSummary(TypedDict):
    learns: list[EditLearnEntry]
    repeatedRemovals: dict[str, int]
    repeatedAdditions: dict[str, int]


class RuleExample(TypedDict):
    slug: str
    receipt: str
    before_snippet: str
    after_snippet: str


class RuleCounterexample(TypedDict):
    slug: str
    receipt: str
    snippet: str


class RuleCandidate(TypedDict):
    id: str
    kind: str
    word: str
    support: int
    minimumSupport: int
    pieces: list[str]
    receipts: list[str]
    sections: list[str]
    origins: list[str]
    examples: list[RuleExample]
    counterexamples: list[RuleCounterexample]


class ConfirmedRule(RuleCandidate):
    scope: str | None
    confirmedAt: str | None


class SuppressedRule(TypedDict):
    id: str
    kind: str
    word: str
    status: Literal["rejected", "retired"]
    at: str | None


class RuleSummary(TypedDict):
    minimumSupport: int
    confirmed: list[ConfirmedRule]
    hypotheses: list[RuleCandidate]
    rejected: list[SuppressedRule]
    retired: list[SuppressedRule]


class RulesState(TypedDict):
    kind: Literal["ghostless-rules"]
    version: Literal[1]
    confirmed: dict[str, dict[str, object]]
    rejected: dict[str, dict[str, object]]
    retired: dict[str, dict[str, object]]
    scope: dict[str, str]


class ChecksState(TypedDict):
    kind: Literal["ghostless-checks"]
    version: Literal[1]
    confirmed: dict[str, dict[str, object]]
    rejected: dict[str, dict[str, object]]


class ChecksSummary(TypedDict):
    confirmed: list[dict[str, object]]
    pending: list[dict[str, object]]
    rejected: list[dict[str, object]]


class CompiledStats(TypedDict):
    schemaVersion: Literal[1]
    weights: dict[str, float]
    goldVoiceMinimum: int
    goldPieces: int
    bronzeFormatOnly: bool
    edited: EditedPoolInfo
    counts: PieceCounts
    provenance: ProvenanceCounts
    files: list[FileRow]
    weighted: MetricBlock
    unweighted: MetricBlock
    byTier: dict[str, MetricBlock]
    sections: dict[str, SectionBlock]
    warnings: list[str]
    edits: EditSummary
    generatedAt: NotRequired[str]
    failUnder: NotRequired[int]
    overrides: NotRequired[dict[str, float]]
    placeholder: NotRequired[str | None]
    rules: NotRequired[RuleSummary]
    checks: NotRequired[ChecksSummary]
    style: NotRequired[str | None]
    audience: NotRequired[str | None]


class JudgeCheck(TypedDict):
    metric: str
    label: str
    target: float | int
    actual: float | int
    low: float
    high: float
    status: Literal["ok", "low", "high", "off"]
    penalty: int


class BannedHit(TypedDict):
    tell: str
    count: int


class ConfirmedRuleHit(TypedDict):
    id: str
    word: str
    scope: str | None
    count: int


class StructureHitDict(TypedDict):
    kind: str
    excerpt: str


class JudgeReport(TypedDict):
    ok: bool
    score: int
    failUnder: int
    reviewRequired: NotRequired[bool]
    assessment: NotRequired[Literal["mechanical-only"]]
    section: str
    checks: list[JudgeCheck]
    banned: list[BannedHit]
    confirmedRules: list[ConfirmedRuleHit]
    structure: list[StructureHitDict]
    placeholders: list[str]
    draft: dict[str, object]
    targets: dict[str, object]
    confidence: CorpusConfidence
    lowConfidence: bool
    stale: NotRequired[bool]
    staleSources: NotRequired[list[str]]
    error: NotRequired[str]


class HoldoutEval(TypedDict):
    pieces: int
    train: dict[str, object]
    summary: dict[str, object]
    items: list[dict[str, object]]
