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

RuleKind = Literal["cut", "add", "struct"]
RuleStatus = Literal["confirmed", "rejected", "retired", "hypothesis", "sparse"]
ConfidenceLevel = Literal["low", "medium", "high"]

STRUCTURAL_RULE_LABELS: dict[str, str] = {
    "cut-em-dash": "Cut em dashes",
    "add-em-dash": "Add em dashes",
    "cut-semicolon": "Cut semicolons",
    "cut-exclamation": "Cut exclamation marks",
    "split-paragraphs": "Split paragraphs",
    "merge-paragraphs": "Merge paragraphs",
}

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
OVERRIDABLE_METRICS = (*BOTH_DIRECTIONS, "emDashesPer1k")

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


StructuralChange = TypedDict(
    "StructuralChange",
    {
        "kind": str,
        "mark": str,
        "before": int,
        "after": int,
        "delta": int,
        "change": str,
        "from": int,
        "to": int,
        "preview": str,
    },
    total=False,
)


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
    structuralChanges: list[StructuralChange]
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


class RuleDecisionRecord(TypedDict, total=False):
    at: str
    receipts: list[str]
    pieces: list[str]


class RuleDecisionResult(TypedDict, total=False):
    id: str
    status: str
    scope: str | None
    receipts: list[str]
    pieces: list[str]
    at: str


class RulesState(TypedDict):
    kind: Literal["ghostless-rules"]
    version: Literal[1]
    confirmed: dict[str, RuleDecisionRecord]
    rejected: dict[str, RuleDecisionRecord]
    retired: dict[str, RuleDecisionRecord]
    scope: dict[str, str]


class CheckRecord(TypedDict, total=False):
    id: str
    text: str
    span: str
    polarity: str
    status: str
    reused: bool
    pair: str | None
    section: str | None
    at: str


class CheckItem(CheckRecord):
    pass


class CheckDecisionResult(CheckRecord):
    pass


class ChecksState(TypedDict):
    kind: Literal["ghostless-checks"]
    version: Literal[1]
    confirmed: dict[str, CheckRecord]
    rejected: dict[str, CheckRecord]


class ChecksSummary(TypedDict):
    confirmed: list[CheckItem]
    pending: list[CheckItem]
    rejected: list[CheckItem]


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


class MetricMap(TypedDict, total=False):
    rhythmWords: float
    rhythmSentences: float
    dictionWords: float
    formatParagraphs: float
    medianSentenceLength: float | int | None
    sentenceLengthDistribution: Distribution
    contractionsPer1k: float | None
    iPer1k: float | None
    emDashesPer1k: float | int | None
    questionsPer1k: float | None
    medianParagraphLength: float | int | None
    openerShare: float | None
    openers: OpenerRates


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
    draft: MetricMap
    targets: MetricMap
    confidence: CorpusConfidence
    lowConfidence: bool
    stale: NotRequired[bool]
    staleSources: NotRequired[list[str]]
    error: NotRequired[str]


class EvalItem(TypedDict, total=False):
    kind: str
    path: str
    slug: str
    section: str | None
    origin: str | None
    before: str
    words: int
    score: int | None
    ok: bool
    editRate: float
    accepted: bool
    error: str


class EvalSummary(TypedDict, total=False):
    pieces: int
    words: int
    writtenWords: int
    sentences: int
    confidence: str
    judged: int
    meanScore: float | None
    minScore: float | None
    maxScore: float | None
    meanEditRate: float | None
    passRate: float | None


class HoldoutSummary(EvalSummary):
    pass


class ScoreGroup(TypedDict):
    count: int
    meanScore: float | None


class AcceptanceSummary(TypedDict):
    threshold: float
    accepted: ScoreGroup
    revised: ScoreGroup
    scoreGap: float | None


class CorrelationSummary(TypedDict):
    pearson: float | None
    spearman: float | None
    n: int


class EvalReport(TypedDict, total=False):
    ok: bool
    mode: str
    cv: bool
    seed: int
    totalCount: int
    holdoutCount: int
    train: EvalSummary | None
    holdout: HoldoutSummary | None
    holdouts: HoldoutEval | None
    summary: EvalSummary
    correlation: CorrelationSummary | None
    acceptance: AcceptanceSummary | None
    direction: str | None
    verdict: str
    items: list[EvalItem]
    notes: list[str]
    confirmedChecks: GradeReport
    error: str
    skillFiles: list[str]


class HoldoutEval(TypedDict):
    pieces: int
    train: EvalSummary
    summary: HoldoutSummary
    items: list[EvalItem]


class SessionMeta(TypedDict):
    kind: str
    version: int
    id: str
    date: str
    createdAt: str
    section: str | None
    origin: str
    sources: list[str]
    sourceDraftName: str
    status: str
    beforeFile: str
    afterFile: str
    notesFile: str
    receiptFile: str
    beforeSha256: str


class SessionStartResult(SessionMeta):
    path: str


class SessionListEntry(TypedDict):
    id: str | None
    createdAt: str | None
    sourceDraftName: str | None
    status: str
    path: str


class SessionSnapshotResult(TypedDict):
    id: str
    path: str
    savedAt: str


class SessionChangesSummary(DiffSummary):
    pass


class SessionReceipt(TypedDict, total=False):
    kind: str
    version: int
    sessionId: str
    finishedAt: str
    origin: str
    section: str | None
    sources: list[str]
    sourceDraftName: str | None
    beforeSha256: str
    finalSha256: str
    beforeFile: str
    finalFile: str
    notesFile: str | None
    notesSha256: str | None
    learnedPath: str
    tier: str | None
    confirmation: str | None
    changes: SessionChangesSummary
    checks: list[CheckItem]
    promotion: LearnResult
    rebuilt: bool
    warnings: list[str]


class ManifestFile(TypedDict):
    kind: Literal["ghostless"]
    version: Literal[1]
    weights: dict[str, float]
    goldVoiceMinimum: int
    updateCheck: bool
    failUnder: int


class LearnResult(TypedDict, total=False):
    slug: str
    path: str
    meta: object
    edits: EditSummary
    origin: str | None
    changed: bool
    confirmation: str
    tier: str
    model: str | None
    tool: str | None
    checks: list[CheckItem]
    warning: str | None
    previous: str
    gold: bool


class HoldoutCase(TypedDict, total=False):
    slug: str
    section: str | None
    before: str


CheckGradeItem = TypedDict(
    "CheckGradeItem",
    {
        "check": str,
        "text": str,
        "slug": str,
        "case": str,
        "pass": bool,
    },
)


class GradeReport(TypedDict):
    passes: int
    total: int
    label: str | None
    session: str | None
    fresh: bool
    skillSha256: str | None
    skillFiles: dict[str, str | None]
    guideSha256: str | None
    items: list[CheckGradeItem]
    note: str | None


BridgeParams = TypedDict(
    "BridgeParams",
    {
        "project": str,
        "path": str,
        "text": str,
        "section": str,
        "action": str,
        "ruleId": str,
        "target": str,
        "global": bool,
        "session": str,
        "after": str,
        "notes": str,
        "final": str,
        "note": str,
        "checks": list[str],
        "slug": str,
        "demote": bool,
        "promote": bool,
        "rebuild": bool,
        "draft": str,
        "origin": str,
        "before": str,
        "date": str,
        "sources": list[str],
    },
    total=False,
)


class ProjectResolveResult(TypedDict):
    project: str | None
    source: str | None


class BridgeRequest(TypedDict, total=False):
    id: str | int | None
    method: str
    params: BridgeParams


class BridgeSessionItem(SessionListEntry, total=False):
    section: str | None
    origin: str | None
    editRate: float | None
    learnedPath: str | None


ConfirmedCheckView = TypedDict(
    "ConfirmedCheckView",
    {
        "id": str,
        "text": str | None,
        "span": str,
        "polarity": str,
        "section": str | None,
        "pass": bool,
    },
)


class BridgeJudgeResult(TypedDict, total=False):
    needsBuild: bool
    error: str
    ok: bool
    score: int
    failUnder: int
    reviewRequired: bool
    assessment: str
    section: str
    checks: list[JudgeCheck]
    banned: list[BannedHit]
    confirmedRules: list[ConfirmedRuleHit]
    structure: list[StructureHitDict]
    placeholders: list[str]
    draft: MetricMap
    targets: MetricMap
    confidence: CorpusConfidence
    lowConfidence: bool
    stale: bool
    staleSources: list[str]
    availableSections: list[str]
    confirmedChecks: list[ConfirmedCheckView]


class BridgeBuildResult(CompiledStats):
    pass


class BridgeResponse(TypedDict, total=False):
    id: str | int | None
    ok: bool
    error: str
    result: object
