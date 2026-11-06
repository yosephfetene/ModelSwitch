//! Is a failed attempt worth another try, and where (ADR-028)?
//!
//! The question behind every case: "if the same request is sent again in
//! a moment, could the answer be different?"

use crate::domain::ProviderError;

/// Gemini's reason for a bad key, which it sends as a 400.
const REJECTED_KEY_CODE: &str = "API_KEY_INVALID";

/// What a failed attempt says about trying again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// A passing glitch (connection failure, 5xx, garbage response):
    /// retry the same target, then fall back.
    Transient,
    /// Something about this provider or model (rate limit, missing model,
    /// bad provider key, timeout): skip to the next candidate.
    TargetProblem,
    /// Something about the request itself (refused by a safety filter,
    /// rejected as invalid): no other provider should receive it either.
    RequestProblem,
}

impl FailureClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::TargetProblem => "target_problem",
            Self::RequestProblem => "request_problem",
        }
    }

    /// May the same target be tried again?
    pub fn retry_same_target(self) -> bool {
        matches!(self, FailureClass::Transient)
    }

    /// May the next candidate be tried?
    pub fn fall_back(self) -> bool {
        matches!(self, FailureClass::Transient | FailureClass::TargetProblem)
    }
}

/// Classifies a provider error. A timeout never reaches here (it is not a
/// `ProviderError`); the executor treats it as a `TargetProblem`.
///
/// For `ProviderError::Status`, decide by the HTTP status number:
///
/// - 500-599: the provider is struggling.
/// - 401, 403, 404, 429: this provider's key, model, or rate limit.
/// - 400 with the code `API_KEY_INVALID`: this provider's key.
/// - any other 400-499: our request was rejected.
/// - anything else (such as a 3xx redirect we refused to follow): a
///   misconfigured target.
pub fn classify(error: &ProviderError) -> FailureClass {
    match error {
        // A network blip or garbage JSON: try again.
        ProviderError::Request(_) => FailureClass::Transient,
        ProviderError::InvalidResponse(_) => FailureClass::Transient,
        // A safety refusal: no other provider should get this prompt.
        ProviderError::Blocked { .. } => FailureClass::RequestProblem,
        ProviderError::Status { status, code } => match status {
            // The provider is struggling.
            500..=599 => FailureClass::Transient,
            // This provider's key, model, or rate limit. Must come BEFORE
            // the 4xx range, or the range would catch these first.
            401 | 403 | 404 | 429 => FailureClass::TargetProblem,
            400 if code.as_deref() == Some(REJECTED_KEY_CODE) => FailureClass::TargetProblem,
            // Our request was rejected.
            400..=499 => FailureClass::RequestProblem,
            // Anything else, such as a redirect we refused to follow.
            _ => FailureClass::TargetProblem,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(status: u16) -> ProviderError {
        ProviderError::Status { status, code: None }
    }

    #[test]
    fn passing_glitches_are_transient() {
        for error in [
            ProviderError::Request("connection refused".to_owned()),
            ProviderError::InvalidResponse("not a chat completion".to_owned()),
            status(500),
            status(502),
            status(503),
            status(504),
        ] {
            assert_eq!(classify(&error), FailureClass::Transient, "{error}");
        }
    }

    #[test]
    fn provider_specific_failures_are_target_problems() {
        for code in [401, 403, 404, 429] {
            assert_eq!(
                classify(&status(code)),
                FailureClass::TargetProblem,
                "{code}"
            );
        }
        // A rate limit with its code is still a rate limit.
        let rate_limited = ProviderError::Status {
            status: 429,
            code: Some("rate_limit_exceeded".to_owned()),
        };
        assert_eq!(classify(&rate_limited), FailureClass::TargetProblem);
    }

    #[test]
    fn a_rejected_key_sent_as_a_400_is_a_target_problem() {
        let rejected_key = ProviderError::Status {
            status: 400,
            code: Some("API_KEY_INVALID".to_owned()),
        };
        assert_eq!(classify(&rejected_key), FailureClass::TargetProblem);
    }

    #[test]
    fn only_a_400_can_be_a_rejected_key() {
        let other_status = ProviderError::Status {
            status: 422,
            code: Some("API_KEY_INVALID".to_owned()),
        };
        assert_eq!(classify(&other_status), FailureClass::RequestProblem);
    }

    #[test]
    fn a_400_with_another_code_is_still_a_request_problem() {
        let invalid_request = ProviderError::Status {
            status: 400,
            code: Some("INVALID_ARGUMENT".to_owned()),
        };
        assert_eq!(classify(&invalid_request), FailureClass::RequestProblem);
    }

    #[test]
    fn unexpected_statuses_are_target_problems() {
        for code in [307, 302, 101] {
            assert_eq!(
                classify(&status(code)),
                FailureClass::TargetProblem,
                "{code}"
            );
        }
    }

    #[test]
    fn rejected_or_refused_requests_are_request_problems() {
        for error in [
            status(400),
            status(413),
            status(422),
            ProviderError::Blocked {
                reason: Some("SAFETY".to_owned()),
            },
            ProviderError::Blocked { reason: None },
        ] {
            assert_eq!(classify(&error), FailureClass::RequestProblem, "{error}");
        }
    }

    #[test]
    fn only_transient_failures_retry_the_same_target() {
        assert!(FailureClass::Transient.retry_same_target());
        assert!(!FailureClass::TargetProblem.retry_same_target());
        assert!(!FailureClass::RequestProblem.retry_same_target());
    }

    #[test]
    fn request_problems_never_fall_back() {
        assert!(FailureClass::Transient.fall_back());
        assert!(FailureClass::TargetProblem.fall_back());
        assert!(
            !FailureClass::RequestProblem.fall_back(),
            "shopping a refused prompt around providers routes around safety filters"
        );
    }
}
