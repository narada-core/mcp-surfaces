impl HiddenCommand for Command {
    fn creation_flags_hidden(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            self.creation_flags(0x08000000);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_selection_and_glide_are_bounded() {
        assert_eq!(selected_providers("all").unwrap(), vec!["codex", "kimi"]);
        assert!(selected_providers("other").is_err());
        let value = attach_glide(
            json!({"windows":[{"usedPercent":50.0,"remainingPercent":50.0,"resetAt":"2026-08-15T00:00:00Z","durationSeconds":86400.0}]}),
        );
        assert!(value["windows"][0]["glidePath"]["status"].is_string());
        assert!(expiry_epoch_seconds(&json!("2026-08-15T00:00:00Z")).is_some());
    }
}
