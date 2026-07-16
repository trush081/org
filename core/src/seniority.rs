//! Title → seniority rank. Pure string heuristics, no DB.
//!
//! Titles are free text, so this can only ever be a best-effort keyword parse.
//! The contract that keeps it honest: return `None` for anything unrecognized
//! rather than guessing, and let callers sort unknowns *last* — a wrong rank
//! misleads, a missing rank just doesn't help.

/// Seniority on one linear scale, IC track below management track.
///
/// Deriving `Ord` on an enum compares variants by *declaration order* (Intern
/// is least, CLevel is greatest), so the ordering lives in this list and
/// nowhere else — reorder the variants and every comparison follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    Intern,
    Junior,
    Mid,
    Senior,
    Staff,
    Principal,
    Lead,
    Manager,
    Director,
    Vp,
    CLevel,
}

/// Parse a job title into a [`Rank`], or `None` if nothing recognizable.
///
/// Matching is on whole lowercased words (so "senior" can't false-match inside
/// another word), checked from the top of the ladder down. Checking top-down is
/// what makes compound titles resolve correctly: "Senior Engineering Manager"
/// hits Manager before Senior ever gets a look.
pub fn rank_title(title: &str) -> Option<Rank> {
    // Split on anything non-alphanumeric: handles "Sr. Engineer", "VP, Sales".
    let words: Vec<String> = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect();

    let has = |options: &[&str]| words.iter().any(|w| options.contains(&w.as_str()));

    if has(&["ceo", "cto", "cfo", "coo", "chief"]) {
        return Some(Rank::CLevel);
    }
    if has(&["vp", "vice"]) {
        return Some(Rank::Vp);
    }
    if has(&["director"]) {
        return Some(Rank::Director);
    }
    if has(&["manager", "mgr"]) {
        return Some(Rank::Manager);
    }
    if has(&["lead"]) {
        return Some(Rank::Lead);
    }
    if has(&["principal"]) {
        return Some(Rank::Principal);
    }
    if has(&["staff"]) {
        return Some(Rank::Staff);
    }
    if has(&["senior", "sr"]) {
        return Some(Rank::Senior);
    }
    if has(&["junior", "jr"]) {
        return Some(Rank::Junior);
    }
    if has(&["intern"]) {
        return Some(Rank::Intern);
    }
    // Ladder levels ("SWE II", "Engineer 3"): the number carries the rank.
    // Checked after the word forms so "Senior Engineer II" resolves as Senior.
    if has(&["v", "5"]) {
        return Some(Rank::Principal);
    }
    if has(&["iv", "4"]) {
        return Some(Rank::Staff);
    }
    if has(&["iii", "3"]) {
        return Some(Rank::Senior);
    }
    if has(&["ii", "2"]) {
        return Some(Rank::Mid);
    }
    if has(&["i", "1"]) {
        return Some(Rank::Junior);
    }
    None
}

/// Rank for an optional title (people may have no title at all).
pub fn rank_of(title: Option<&str>) -> Option<Rank> {
    title.and_then(rank_title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_orders_ic_below_management() {
        assert!(Rank::Intern < Rank::Junior);
        assert!(Rank::Mid < Rank::Senior);
        assert!(Rank::Senior < Rank::Staff);
        assert!(Rank::Principal < Rank::Lead);
        assert!(Rank::Manager < Rank::Director);
        assert!(Rank::Vp < Rank::CLevel);
    }

    #[test]
    fn common_titles_parse() {
        assert_eq!(rank_title("Sr Engineer"), Some(Rank::Senior));
        assert_eq!(rank_title("Senior Software Engineer"), Some(Rank::Senior));
        assert_eq!(rank_title("SWE II"), Some(Rank::Mid));
        assert_eq!(rank_title("SWE I"), Some(Rank::Junior));
        assert_eq!(rank_title("Engineer 3"), Some(Rank::Senior));
        assert_eq!(rank_title("Staff Engineer"), Some(Rank::Staff));
        assert_eq!(rank_title("Principal Analyst"), Some(Rank::Principal));
        assert_eq!(rank_title("Engineering Manager"), Some(Rank::Manager));
        assert_eq!(rank_title("Director"), Some(Rank::Director));
        assert_eq!(rank_title("VP, Sales"), Some(Rank::Vp));
        assert_eq!(rank_title("CTO"), Some(Rank::CLevel));
        assert_eq!(rank_title("Engineering Intern"), Some(Rank::Intern));
    }

    #[test]
    fn compound_titles_take_the_higher_track() {
        // Manager outranks the "Senior" qualifier.
        assert_eq!(rank_title("Senior Engineering Manager"), Some(Rank::Manager));
        assert_eq!(rank_title("Sr Director of Product"), Some(Rank::Director));
        // Word form beats the trailing ladder number.
        assert_eq!(rank_title("Senior Engineer II"), Some(Rank::Senior));
    }

    #[test]
    fn whole_word_matching_no_substring_hits() {
        // "sr" must not match inside another word.
        assert_eq!(rank_title("NSR Specialist"), None);
        // "Interning" is not "intern" (whole-word only).
        assert_eq!(rank_title("Interning Specialist"), None);
    }

    #[test]
    fn unrecognized_is_none_not_a_guess() {
        assert_eq!(rank_title("Analyst"), None);
        assert_eq!(rank_title("Engineer"), None);
        assert_eq!(rank_of(None), None);
        assert_eq!(rank_of(Some("Wizard of Vibes")), None);
    }
}
