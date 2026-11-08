//! The colours a desktop gave, role by role. A source may give some and not others.

use gpui::Hsla;

/// What one source (a stylesheet, a settings file) says about the four roles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Roles {
    pub surface: Option<Hsla>,
    pub on_surface: Option<Hsla>,
    pub accent: Option<Hsla>,
    pub danger: Option<Hsla>,
}

impl Roles {
    /// These roles, with `fallback`'s for the ones missing here.
    #[must_use]
    pub fn or(self, fallback: Self) -> Self {
        Self {
            surface: self.surface.or(fallback.surface),
            on_surface: self.on_surface.or(fallback.on_surface),
            accent: self.accent.or(fallback.accent),
            danger: self.danger.or(fallback.danger),
        }
    }

    /// Whether any source gave anything at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.surface.is_none() && self.on_surface.is_none() && self.accent.is_none() && self.danger.is_none()
    }
}

/// The sources in order of trust: the first to give a role decides it.
#[must_use]
pub fn merged(sources: &[Roles]) -> Roles {
    sources
        .iter()
        .fold(Roles::default(), |so_far, next| so_far.or(*next))
}

#[cfg(test)]
mod tests {
    use gpui::{black, white};

    use super::*;

    #[test]
    fn a_role_missing_here_comes_from_the_fallback() {
        let here = Roles {
            accent: Some(white()),
            ..Roles::default()
        };
        let fallback = Roles {
            surface: Some(black()),
            ..Roles::default()
        };
        assert_eq!(here.or(fallback).surface, Some(black()));
    }

    #[test]
    fn a_role_given_here_beats_the_fallback() {
        let here = Roles {
            accent: Some(white()),
            ..Roles::default()
        };
        let fallback = Roles {
            accent: Some(black()),
            ..Roles::default()
        };
        assert_eq!(here.or(fallback).accent, Some(white()));
    }

    #[test]
    fn the_first_source_to_give_a_role_decides_it() {
        let first = Roles {
            danger: Some(white()),
            ..Roles::default()
        };
        let second = Roles {
            danger: Some(black()),
            ..Roles::default()
        };
        assert_eq!(merged(&[first, second]).danger, Some(white()));
    }

    #[test]
    fn no_sources_give_empty_roles() {
        assert!(merged(&[]).is_empty());
    }
}
