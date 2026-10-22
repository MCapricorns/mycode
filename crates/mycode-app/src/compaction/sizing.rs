//! Sizing requests and compactions in provider tokens.
//!
//! A request's size is the provider's own count of the request the turn
//! sent last (the usage on its reply) plus an estimate of what the turn
//! added since: the reply itself, tool results, steers. Only a turn's first
//! request is estimated whole, scaled by how far the provider's count ran
//! from the estimate the last time both were known. OpenCode and pi size
//! requests the same way.

use std::sync::Arc;

use mycode_agent::{ContentBlock, Message, ToolSpec, Usage};

/// Fallback trigger when the catalog/settings have no context window.
const DEFAULT_THRESHOLD_TOKENS: usize = 48_000;
/// Usable window as a percent of the raw model context (Codex 95%).
const USABLE_WINDOW_PERCENT: u64 = 95;
/// Auto-compact trigger as a percent of the usable window (Codex 90%).
const TRIGGER_PERCENT: u64 = 90;
/// Most tokens kept verbatim after the summary; Codex keeps about this many.
const MAX_TAIL_TOKENS: usize = 20_000;
/// Fewest tokens kept verbatim, however small the window.
const MIN_TAIL_TOKENS: usize = 2_000;
/// Longest summary the prompt asks for, in words.
const MAX_SUMMARY_WORDS: usize = 2_500;
/// Shortest summary the prompt asks for, in words.
const MIN_SUMMARY_WORDS: usize = 400;
/// Tokens set aside per summary word: a CJK word is about one token, an
/// English word about 1.3.
const TOKENS_PER_SUMMARY_WORD: usize = 2;
/// One image, near what providers bill a typical screenshot.
const IMAGE_TOKENS: usize = 1_600;
/// Role markers and separators around one message.
const MESSAGE_FRAME_TOKENS: usize = 8;
/// Least output cap a turn request keeps when it is lowered to fit.
const MIN_OUTPUT_TOKENS: u64 = 4_096;

/// Tokens that fire auto-compaction for this model window.
#[must_use]
pub(crate) fn compaction_threshold(context_window: u64) -> usize {
    if context_window == 0 {
        return DEFAULT_THRESHOLD_TOKENS;
    }
    let usable = context_window.saturating_mul(USABLE_WINDOW_PERCENT) / 100;
    (usable.saturating_mul(TRIGGER_PERCENT) / 100) as usize
}

/// Tokens one request may hold: 95% of the window, or the window the
/// fallback trigger implies when the real one is unknown.
pub(super) fn usable_window(context_window: u64) -> u64 {
    if context_window == 0 {
        return DEFAULT_THRESHOLD_TOKENS as u64 * 100 / TRIGGER_PERCENT;
    }
    context_window.saturating_mul(USABLE_WINDOW_PERCENT) / 100
}

/// Token sizes for one compaction, from the model window and the prefix
/// every request carries. All sizes are provider tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Budget {
    /// Request size above which auto-compaction fires.
    pub(super) threshold: usize,
    /// Newest tokens kept verbatim after the summary.
    pub(super) tail: usize,
    /// Most tokens of the summarized head's user messages kept verbatim in
    /// the summary.
    pub(super) echo: usize,
    /// Length the summary prompt asks for, in words.
    pub(super) summary_words: usize,
}

impl Budget {
    /// `overhead` is the calibrated size of the system prompt and tools.
    ///
    /// A fixed 20k tail left a small window over the trigger right after
    /// compacting, so every request summarized again. The room under the
    /// trigger is split instead: the summary first, then half of what is
    /// left for the tail and half free for the conversation to grow into.
    /// The user's own words from the summarized head take a quarter of the
    /// tail's share out of that free half.
    pub(super) fn new(context_window: u64, overhead: usize) -> Self {
        let threshold = compaction_threshold(context_window);
        let room = threshold.saturating_sub(overhead);
        let summary_words = (room / 6).clamp(MIN_SUMMARY_WORDS, MAX_SUMMARY_WORDS);
        let tail = (room.saturating_sub(summary_words * TOKENS_PER_SUMMARY_WORD) / 2)
            .clamp(MIN_TAIL_TOKENS, MAX_TAIL_TOKENS);
        Self {
            threshold,
            tail,
            echo: tail / 4,
            summary_words,
        }
    }

    /// Tokens the summary may take, generously.
    pub(super) fn summary_tokens(self) -> usize {
        self.summary_words * TOKENS_PER_SUMMARY_WORD
    }

    /// Fewest tokens an automatic compaction must free beyond the summary
    /// it adds. Summarizing a head barely larger than its summary buys
    /// nothing, and in a small window the next request summarized again.
    pub(super) fn min_gain(self) -> usize {
        (self.threshold / 10).max(MIN_TAIL_TOKENS)
    }
}

/// How many provider tokens one estimated token turned out to be, in
/// thousandths: the provider's count of a request over this module's
/// estimate of it. Bounded, so one odd reading cannot swing sizing far.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TokenScale(usize);

impl TokenScale {
    pub(crate) const ONE: Self = Self(1_000);
    const MIN: usize = 750;
    const MAX: usize = 3_000;

    /// The scale that turns `estimate` into the `reported` count.
    pub(super) fn measured(reported: u64, estimate: usize) -> Self {
        if reported == 0 || estimate == 0 {
            return Self::ONE;
        }
        let permille = u128::from(reported) * 1_000 / estimate as u128;
        Self(
            usize::try_from(permille)
                .map_or(Self::MAX, |permille| permille.clamp(Self::MIN, Self::MAX)),
        )
    }

    /// `tokens` estimated, as the provider would count them.
    pub(crate) fn apply(self, tokens: usize) -> usize {
        tokens.saturating_mul(self.0) / 1_000
    }
}

impl Default for TokenScale {
    fn default() -> Self {
        Self::ONE
    }
}

/// What a turn knows about the size of the request it is about to send.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Measure {
    pub scale: TokenScale,
    /// The provider's count of the turn's previous request plus the
    /// estimate of everything added since; `None` before the first reply.
    pub tokens: Option<usize>,
}

/// A turn's running measurement, kept by its request hook.
///
/// The hook knows exactly which messages it sent, and the reply to them is
/// the next message in the history. A reading taken before a compaction
/// therefore never sizes the compacted request: the request after a
/// compaction is measured by its own reply.
#[derive(Clone, Default)]
pub(crate) struct Calibration {
    state: Arc<parking_lot::Mutex<CalibrationState>>,
}

#[derive(Default)]
struct CalibrationState {
    /// Message count of the request sent last, and its estimate with the
    /// prefix.
    sent: Option<(usize, usize)>,
    scale: TokenScale,
}

impl Calibration {
    /// A calibration that starts from an earlier turn's scale.
    pub(crate) fn new(scale: TokenScale) -> Self {
        let calibration = Self::default();
        calibration.state.lock().scale = scale;
        calibration
    }

    /// Sizes the request about to go out, from `messages`, the turn's
    /// history before it is compacted.
    pub(crate) fn measure(&self, messages: &[Arc<Message>]) -> Measure {
        let mut state = self.state.lock();
        if let Some((count, estimate)) = state.sent.take()
            && let Some(Message::Assistant(reply)) = messages.get(count).map(AsRef::as_ref)
            && let Some(reported) = reply
                .usage
                .as_ref()
                .map(reported_prompt)
                .filter(|tokens| *tokens > 0)
        {
            let scale = TokenScale::measured(reported, estimate);
            state.scale = scale;
            let since: usize = messages[count..]
                .iter()
                .map(|message| scale.apply(message_tokens(message)))
                .sum();
            // A count far under the estimate is more often a gateway's
            // partial figure than a frugal tokenizer; the request is never
            // taken for smaller than the scale's floor allows.
            let counted = usize::try_from(reported)
                .unwrap_or(usize::MAX)
                .max(scale.apply(estimate));
            return Measure {
                scale,
                tokens: Some(counted.saturating_add(since)),
            };
        }
        Measure {
            scale: state.scale,
            tokens: None,
        }
    }

    /// Records the request about to go out, so its reply can measure it.
    pub(crate) fn sent(&self, messages: &[Arc<Message>], overhead: usize) {
        let estimate = overhead.saturating_add(estimate_messages(messages));
        self.state.lock().sent = Some((messages.len(), estimate));
    }

    /// The provider refused the request sent last as over the window, so
    /// it held at least the usable window: estimates are scaled up to say
    /// so. The refused request has no reply to measure it.
    pub(crate) fn overflowed(&self, context_window: u64) {
        let mut state = self.state.lock();
        if let Some((_, estimate)) = state.sent {
            let floor = TokenScale::measured(usable_window(context_window), estimate);
            state.scale = state.scale.max(floor);
        }
    }

    /// The latest scale, for the session's next turn.
    pub(crate) fn scale(&self) -> TokenScale {
        self.state.lock().scale
    }
}

/// The prompt size a reply's usage reports, cache reads included.
fn reported_prompt(usage: &Usage) -> u64 {
    if usage.prompt_tokens > 0 {
        usage.prompt_tokens
    } else {
        usage.input_tokens
    }
}

/// Lowers a request's output cap to what the window leaves after a request
/// of `tokens`. Anthropic and OpenAI-style providers reject a request whose
/// input and output cap together pass the window, which with a 32k or 64k
/// cap happened well before compaction fired.
#[must_use]
pub(crate) fn fit_output(cap: Option<u64>, context_window: u64, tokens: usize) -> Option<u64> {
    let cap = cap?;
    if context_window == 0 {
        return Some(cap);
    }
    let margin = (context_window / 50).max(1_000);
    let room = context_window
        .saturating_sub(u64::try_from(tokens).unwrap_or(u64::MAX))
        .saturating_sub(margin);
    Some(cap.min(room.max(MIN_OUTPUT_TOKENS)))
}

/// Rough provider-token count of text and images.
///
/// Four ASCII characters make about a token. Every other character counts
/// as a token of its own: CJK runs near a token a character on most
/// tokenizers, and counting it like English under-counted Chinese sessions
/// about fourfold, so they overflowed before compaction fired.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Tally {
    ascii: usize,
    other: usize,
    images: usize,
}

impl Tally {
    pub(super) fn text(&mut self, text: &str) {
        for byte in text.bytes() {
            if byte.is_ascii() {
                self.ascii += 1;
            } else if byte >= 0xC0 {
                // A lead byte starts each non-ASCII character.
                self.other += 1;
            }
        }
    }

    pub(super) fn char(&mut self, ch: char) {
        if ch.is_ascii() {
            self.ascii += 1;
        } else {
            self.other += 1;
        }
    }

    pub(super) fn tokens(self) -> usize {
        self.ascii.div_ceil(4) + self.other + self.images * IMAGE_TOKENS
    }
}

pub(super) fn text_tokens(text: &str) -> usize {
    let mut tally = Tally::default();
    tally.text(text);
    tally.tokens()
}

/// Estimated tokens for the system prompt and tool schemas sent with every
/// request.
#[must_use]
pub(crate) fn estimate_prefix_tokens(system: &[String], tools: &[ToolSpec]) -> usize {
    let mut tally = Tally::default();
    for part in system {
        tally.text(part);
    }
    for tool in tools {
        tally.text(&tool.name);
        tally.text(&tool.description);
        tally_json(&mut tally, &tool.params_schema);
    }
    tally.tokens()
}

/// Estimated tokens for a run of messages.
pub(super) fn estimate_messages(messages: &[Arc<Message>]) -> usize {
    messages.iter().map(|message| message_tokens(message)).sum()
}

/// Estimated tokens for one message as the wire sends it.
pub(super) fn message_tokens(message: &Message) -> usize {
    let mut tally = Tally::default();
    match message {
        Message::User(user) => tally_blocks(&mut tally, &user.content),
        Message::Assistant(assistant) => tally_blocks(&mut tally, &assistant.blocks),
        Message::ToolResult(result) => {
            // `tool_result {id} {body}`
            tally.text("tool_result ");
            tally.text(&result.tool_call_id);
            tally.char(' ');
            tally_blocks(&mut tally, &result.content);
        }
    }
    tally.tokens() + MESSAGE_FRAME_TOKENS
}

fn tally_blocks(tally: &mut Tally, blocks: &[ContentBlock]) {
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            tally.char('\n');
        }
        match block {
            ContentBlock::Text(text) => tally.text(&text.text),
            ContentBlock::ToolCall(call) => {
                // `tool_call {name} {arguments}`
                tally.text("tool_call ");
                tally.text(&call.name);
                tally.char(' ');
                tally_json(tally, &call.arguments);
            }
            // Most wires replay reasoning to the provider.
            ContentBlock::Thinking(thinking) => tally.text(&thinking.text),
            ContentBlock::Image(_) => tally.images += 1,
        }
    }
}

fn tally_json(tally: &mut Tally, value: &serde_json::Value) {
    match value {
        // Quoted and escaped the way the wire spells it, without the copy.
        serde_json::Value::String(text) => {
            tally.text(text);
            tally.ascii += 2 + text.chars().map(json_escape_extra).sum::<usize>();
        }
        other => tally.text(&other.to_string()),
    }
}

/// Characters a JSON string escape adds for `ch`.
fn json_escape_extra(ch: char) -> usize {
    match ch {
        '"' | '\\' | '\u{0008}' | '\u{000c}' | '\n' | '\r' | '\t' => 1,
        control if (control as u32) < 0x20 => 5,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mycode_agent::{
        AssistantMessage, ContentBlock, Message, StopReason, TextBlock, Usage, UserMessage,
    };

    use super::{
        Budget, Calibration, Measure, TokenScale, fit_output, message_tokens, text_tokens,
    };

    fn user(text: &str) -> Arc<Message> {
        Arc::new(Message::User(UserMessage::text(text)))
    }

    fn reply(text: &str, prompt_tokens: u64) -> Arc<Message> {
        Arc::new(Message::Assistant(AssistantMessage {
            blocks: vec![ContentBlock::Text(TextBlock::new(text))],
            usage: Some(Usage {
                input_tokens: prompt_tokens,
                output_tokens: 10,
                cache_read_tokens: None,
                cache_write_tokens: None,
                prompt_tokens,
            }),
            stop_reason: StopReason::Stop,
        }))
    }

    #[test]
    fn chinese_text_counts_about_a_token_a_character() {
        assert_eq!(text_tokens("abcdefgh"), 2);
        // A token is about four ASCII characters, and about one CJK one.
        assert_eq!(text_tokens("上下文压缩会丢消息"), 9);
        assert_eq!(text_tokens("压缩 ok"), 2 + 1);
    }

    #[test]
    fn the_reply_to_the_last_request_measures_the_next_one() {
        let calibration = Calibration::default();
        let sent = vec![user(&"a".repeat(4_000))];
        // Nothing sent yet: the first request is only estimated.
        assert_eq!(calibration.measure(&sent).tokens, None);
        calibration.sent(&sent, 100);
        let estimate = 100 + message_tokens(&sent[0]);
        // The provider counted twice the estimate.
        let reported = 2 * estimate as u64;
        let mut next = sent.clone();
        next.push(reply("done", reported));
        next.push(user("tool output"));
        let measure = calibration.measure(&next);
        assert_eq!(measure.scale, TokenScale::measured(reported, estimate));
        let since = message_tokens(&next[1]) + message_tokens(&next[2]);
        assert_eq!(
            measure.tokens,
            Some(reported as usize + measure.scale.apply(since)),
            "the count of the request sent, plus what came after it"
        );
        // One reading per request: the next hook without a new send has
        // nothing measured, only the scale.
        assert_eq!(
            calibration.measure(&next),
            Measure {
                scale: measure.scale,
                tokens: None
            }
        );
    }

    #[test]
    fn an_overflow_scales_estimates_up_to_the_window() {
        let calibration = Calibration::default();
        let sent = vec![user(&"a".repeat(200_000))];
        calibration.sent(&sent, 0);
        let estimate = message_tokens(&sent[0]);
        calibration.overflowed(100_000);
        assert!(
            calibration.scale().apply(estimate) >= 95_000 * 99 / 100,
            "{:?} still sizes a refused {estimate}-token request under the window",
            calibration.scale()
        );
        // A larger scale already measured is kept.
        let calibration = Calibration::new(TokenScale(2_500));
        calibration.sent(&sent, 0);
        calibration.overflowed(100_000);
        assert_eq!(calibration.scale(), TokenScale(2_500));
    }

    #[test]
    fn a_count_far_under_the_estimate_is_not_trusted_whole() {
        let calibration = Calibration::default();
        let sent = vec![user(&"a".repeat(40_000))];
        calibration.sent(&sent, 0);
        let estimate = message_tokens(&sent[0]);
        let mut next = sent.clone();
        next.push(reply("done", 20));
        let measure = calibration.measure(&next);
        let tokens = measure.tokens.expect("measured");
        assert!(
            tokens >= estimate * 3 / 4,
            "{tokens} sized a {estimate}-token request by a 20-token count"
        );
    }

    #[test]
    fn a_compaction_since_the_send_is_not_measured_by_the_old_reply() {
        let calibration = Calibration::default();
        let sent = vec![user("question")];
        calibration.sent(&sent, 0);
        // The history was compacted before the reply was appended where the
        // hook expects it: no reply at that index, no reading.
        let compacted = vec![user("COMPACTION SUMMARY\n\nnotes")];
        assert_eq!(calibration.measure(&compacted).tokens, None);
    }

    #[test]
    fn the_scale_stays_within_its_bounds() {
        assert_eq!(TokenScale::measured(10_000, 1_000).apply(1_000), 3_000);
        assert_eq!(TokenScale::measured(100, 1_000).apply(1_000), 750);
        assert_eq!(TokenScale::measured(0, 1_000), TokenScale::ONE);
    }

    #[test]
    fn a_small_window_keeps_a_tail_that_leaves_room_to_grow() {
        let large = Budget::new(200_000, 10_000);
        assert_eq!(large.tail, 20_000);
        assert_eq!(large.summary_words, 2_500);
        // A 32k window with 10k of prompt and tools: summary plus tail must
        // land well under the trigger, or the next request compacts again.
        let small = Budget::new(32_000, 10_000);
        assert!(
            10_000 + small.summary_tokens() + small.tail + small.echo + small.min_gain()
                <= small.threshold,
            "{small:?}"
        );
        let tiny = Budget::new(16_000, 10_000);
        assert_eq!(tiny.tail, 2_000);
        assert!(tiny.summary_words < 2_500, "{tiny:?}");
    }

    #[test]
    fn the_output_cap_shrinks_to_the_room_the_window_leaves() {
        assert_eq!(fit_output(Some(64_000), 200_000, 100_000), Some(64_000));
        assert_eq!(fit_output(Some(64_000), 200_000, 160_000), Some(36_000));
        assert_eq!(fit_output(Some(64_000), 200_000, 199_000), Some(4_096));
        assert_eq!(fit_output(Some(2_000), 200_000, 199_000), Some(2_000));
        assert_eq!(fit_output(Some(64_000), 0, 500_000), Some(64_000));
        assert_eq!(fit_output(None, 200_000, 100), None);
    }
}
