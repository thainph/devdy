//! BR-07 "ball in my court" for Slack threads (pure).
//!
//! The last meaningful message — not from a bot, not a join/leave notice, not
//! emoji-only — decides: when someone else wrote it and it mentions nobody
//! but (possibly) me, the ball is mine since that message. Otherwise the AI's
//! `ball` stands.

use super::content::SlackMsg;
use crate::slack_threads::mrkdwn;

/// Subtypes that are noise, not conversation.
const NOISE_SUBTYPES: [&str; 13] = [
    "bot_message",
    "tombstone",
    "channel_join",
    "channel_leave",
    "group_join",
    "group_leave",
    "member_joined_channel",
    "member_left_channel",
    "channel_topic",
    "channel_purpose",
    "channel_name",
    "pinned_item",
    "reminder_add",
];

/// Pictographic code points (approximating `Extended_Pictographic`) plus
/// regional indicators (flags), skin tones and tag characters.
fn is_pictographic(c: char) -> bool {
    matches!(c as u32,
        0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x2194..=0x21AA
        | 0x231A..=0x23FF | 0x24C2 | 0x25AA..=0x25FE | 0x2600..=0x27BF
        | 0x2934 | 0x2935 | 0x2B05..=0x2B55 | 0x3030 | 0x303D | 0x3297 | 0x3299
        | 0x1F000..=0x1FAFF | 0xE0020..=0xE007F)
}

/// Joiners and modifiers that only glue pictographs together.
fn is_emoji_glue(c: char) -> bool {
    matches!(c as u32, 0x200D | 0xFE0E | 0xFE0F | 0x20E3)
}

/// Text made only of emoji — `:emoji:` codes (incl. `::skin-tone-2`) and
/// Unicode emoji (ZWJ sequences, skin tones, flags, keycaps like 1️⃣) — and
/// whitespace.
pub fn is_emoji_only(text: &str) -> bool {
    let mut rest = text.trim();
    if rest.is_empty() {
        return false;
    }
    let mut any = false;
    while let Some(c) = rest.chars().next() {
        if c.is_whitespace() || is_emoji_glue(c) {
            rest = &rest[c.len_utf8()..];
            continue;
        }
        if is_pictographic(c) {
            any = true;
            rest = &rest[c.len_utf8()..];
            continue;
        }
        // Keycap: [0-9#*] followed by FE0F and/or 20E3.
        if c.is_ascii_digit() || c == '#' || c == '*' {
            let next = rest[1..].chars().next();
            if next.is_some_and(is_emoji_glue) {
                any = true;
                rest = &rest[1..];
                continue;
            }
            return false;
        }
        if c == ':' {
            let after = &rest[1..];
            let Some(end) = after.find(':') else {
                return false;
            };
            let name = &after[..end];
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '\''))
            {
                return false;
            }
            any = true;
            rest = &after[end + 1..];
            continue;
        }
        return false;
    }
    any
}

fn is_meaningful(m: &SlackMsg) -> bool {
    if m.bot_id.is_some() {
        return false;
    }
    if m.subtype
        .as_deref()
        .is_some_and(|s| NOISE_SUBTYPES.contains(&s))
    {
        return false;
    }
    let text = m.text_raw.trim();
    !text.is_empty() && !is_emoji_only(text)
}

/// Mentions anyone other than `me`: `<@U…>` of someone else, or a broadcast
/// (`<!here>`, `<!channel>`, `<!everyone>`, `<!subteam^…>`).
fn mentions_others(text: &str, me: &str) -> bool {
    if mrkdwn::mentioned_users(text).iter().any(|u| u != me) {
        return true;
    }
    ["<!here", "<!channel", "<!everyone", "<!subteam^"]
        .iter()
        .any(|p| text.contains(p))
}

/// `ts` of the message that put the ball in my court, if any.
pub fn ball_mine_since(msgs: &[SlackMsg], me: &str) -> Option<String> {
    if me.is_empty() {
        return None;
    }
    let last = msgs.iter().rev().find(|m| is_meaningful(m))?;
    if last.user_id.as_deref() == Some(me) || mentions_others(&last.text_raw, me) {
        return None;
    }
    Some(last.ts.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(ts: &str, user: &str, text: &str) -> SlackMsg {
        SlackMsg {
            ts: ts.into(),
            user_id: Some(user.into()),
            author: user.into(),
            text_raw: text.into(),
            ..Default::default()
        }
    }

    const ME: &str = "UME";

    #[test]
    fn last_message_from_someone_else_is_mine() {
        let msgs = [
            m("1.1", ME, "Báo giá đây"),
            m("2.1", "UCUST", "Khi nào có demo?"),
        ];
        assert_eq!(ball_mine_since(&msgs, ME), Some("2.1".into()));
        // Mentioning only me still counts.
        let msgs = [m("1.1", "UCUST", "<@UME> please check")];
        assert_eq!(ball_mine_since(&msgs, ME), Some("1.1".into()));
        let msgs = [m("1.1", "UCUST", "<@UME|thai> please check")];
        assert_eq!(ball_mine_since(&msgs, ME), Some("1.1".into()));
    }

    #[test]
    fn my_own_last_message_is_not_mine_to_answer() {
        let msgs = [m("1.1", "UCUST", "Question?"), m("2.1", ME, "Answer")];
        assert_eq!(ball_mine_since(&msgs, ME), None);
    }

    #[test]
    fn mentioning_someone_else_or_broadcast_is_not_mine() {
        for text in [
            "<@UBOB> can you check?",
            "<@UME> <@UBOB> please",
            "<!here> deploy done?",
            "<!channel> heads up",
            "<!everyone> meeting",
            "<!subteam^S123|@devs> review",
        ] {
            let msgs = [m("1.1", "UCUST", text)];
            assert_eq!(ball_mine_since(&msgs, ME), None, "{text}");
        }
    }

    #[test]
    fn noise_is_skipped() {
        let mut bot = m("3.1", "UBOT", "Build passed");
        bot.bot_id = Some("B1".into());
        let mut bot_sub = m("3.2", "", "Deployed");
        bot_sub.user_id = None;
        bot_sub.subtype = Some("bot_message".into());
        let mut join = m("3.3", "UNEW", "<@UNEW> has joined the channel");
        join.subtype = Some("channel_join".into());
        let msgs = [
            m("1.1", "UCUST", "Can we move the demo?"),
            bot,
            bot_sub,
            join,
            m("3.4", "UBOB", ":+1:"),
            m("3.5", "UBOB", ":thumbsup::skin-tone-2:  :tada:"),
            m("3.6", "UBOB", "   "),
        ];
        assert_eq!(ball_mine_since(&msgs, ME), Some("1.1".into()));
        // Noise after my own message does not hand the ball back to me.
        let msgs = [m("1.1", ME, "Done"), m("2.1", "UCUST", ":pray:")];
        assert_eq!(ball_mine_since(&msgs, ME), None);
    }

    #[test]
    fn edge_cases() {
        assert_eq!(ball_mine_since(&[], ME), None);
        assert_eq!(ball_mine_since(&[m("1.1", "UCUST", "hi")], ""), None);
        let mut anon = m("1.1", "", "hello?");
        anon.user_id = None;
        assert_eq!(ball_mine_since(&[anon], ME), Some("1.1".into()));
        assert!(is_emoji_only(":ok_hand:"));
        assert!(!is_emoji_only("ok :ok_hand:"));
        assert!(!is_emoji_only("10:30"));
        assert!(!is_emoji_only(""));
        assert!(!is_emoji_only("::"));
    }

    #[test]
    fn unicode_emoji_replies_are_noise() {
        for text in [
            "🙏",
            "👍🏻",
            "🙇‍♂️",
            "🇻🇳",
            "1️⃣",
            "#️⃣",
            "❤️",
            "👨‍👩‍👧‍👦",
            "🎉 :tada: 👍",
            " 🙏🙏 ",
            "✅",
        ] {
            assert!(is_emoji_only(text), "{text}");
        }
        for text in ["ok 👍", "👍 ok", "1", "10:30", "ありがとう🙏", "₫", "‍"] {
            assert!(!is_emoji_only(text), "{text}");
        }
        // A thumbs-up after the customer's question does not take the ball back.
        let msgs = [m("1.1", "UCUST", "Khi nào demo?"), m("2.1", "UCUST", "🙏")];
        assert_eq!(ball_mine_since(&msgs, ME), Some("1.1".into()));
        let mut gone = m("3.1", "UCUST", "This message was deleted.");
        gone.subtype = Some("tombstone".into());
        let msgs = [m("1.1", ME, "Done"), gone];
        assert_eq!(ball_mine_since(&msgs, ME), None);
    }
}
