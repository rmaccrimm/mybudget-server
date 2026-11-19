//! Plan file names (SPEC 15.2, 15.3).

/// Longest safe name, in bytes of UTF-8 (SPEC 15.3).
const MAX_SAFE_NAME: usize = 200;

/// A plan's name made safe for a file name (SPEC 15.3).
pub fn safe_name(name: &str) -> String {
    let replaced: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_control() || r#"/\:*?"<>|"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cut = &replaced[..replaced.floor_char_boundary(MAX_SAFE_NAME)];
    // char::is_whitespace is Unicode's White_Space property.
    let trimmed = cut.trim_matches(|c: char| c.is_whitespace() || c == '.');
    if trimmed.is_empty() {
        "Plan".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// The names a plan's file may have, best first: its safe name, then with the
/// first 8 characters of its id for when that's taken, then with the whole
/// id for when both are (SPEC 15.2).
pub fn file_names(plan_name: &str, plan_id: &str) -> [String; 3] {
    let safe = safe_name(plan_name);
    let short_id: String = plan_id.chars().take(8).collect();
    [
        format!("{safe}.sqlite"),
        format!("{safe} ({short_id}).sqlite"),
        format!("{safe} ({plan_id}).sqlite"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_names() {
        assert_eq!(safe_name("Household"), "Household");
        assert_eq!(safe_name("Bills/Rent: 2027"), "Bills_Rent_ 2027");
        assert_eq!(safe_name(" .hidden. "), "hidden");
        assert_eq!(safe_name("..."), "Plan");
        assert_eq!(safe_name(" \u{3000}. "), "Plan");
        assert_eq!(
            safe_name("a\tb\u{7f}c\\d*e?f\"g<h>i|j"),
            "a_b_c_d_e_f_g_h_i_j"
        );
        // C1 controls aren't replaced.
        assert_eq!(safe_name("a\u{85}b"), "a\u{85}b");
        // A control character at an end is replaced before trimming, so it stays.
        assert_eq!(safe_name("\nPlan "), "_Plan");
    }

    #[test]
    fn long_names_are_cut_on_a_character_boundary() {
        // 100 four-byte characters: 400 bytes, cut to 50 characters.
        let name = "😀".repeat(100);
        assert_eq!(safe_name(&name), "😀".repeat(50));
        // 199 bytes then a two-byte character: the whole character won't fit.
        let name = format!("{}é", "a".repeat(199));
        assert_eq!(safe_name(&name), "a".repeat(199));
        // Cutting happens before trimming.
        let name = format!("{}.{}", "a".repeat(199), "b".repeat(50));
        assert_eq!(safe_name(&name), "a".repeat(199));
    }

    #[test]
    fn file_names_add_the_id_when_taken() {
        let id = "3f9a2c1e-7b4d-4e8a-9c2f-1d6b5a8e0f37";
        assert_eq!(
            file_names("Household", id),
            [
                "Household.sqlite".to_owned(),
                "Household (3f9a2c1e).sqlite".to_owned(),
                format!("Household ({id}).sqlite")
            ]
        );
    }
}
