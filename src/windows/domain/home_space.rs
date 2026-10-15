//! Which of the spaces the window server places a window on is its home.

use rini_core::ids::SpaceId;

/// The first of a window's `spaces` the window server calls a user space, else the first of them.
/// Each space comes with whether it is a user space, `None` where the window server gave no answer.
///
/// During Mission Control or a fullscreen transition the window server places a window on its home
/// space and a transient fullscreen one, in either order: preferring a user space keeps a Desktop
/// window from being taken for the fullscreen space's. Lazy: no space past the first user space is
/// asked about.
pub fn home_space(spaces: impl IntoIterator<Item = (SpaceId, Option<bool>)>) -> Option<SpaceId> {
    let mut first = None;
    for (space, user) in spaces {
        if user == Some(true) {
            return Some(space);
        }
        first.get_or_insert(space);
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space(id: u64) -> SpaceId { SpaceId::new(id) }

    #[test]
    fn a_user_space_is_home_wherever_it_is_listed() {
        assert_eq!(
            home_space([(space(9), Some(false)), (space(4), Some(true))]),
            Some(space(4))
        );
        assert_eq!(
            home_space([(space(4), Some(true)), (space(9), Some(true))]),
            Some(space(4)),
            "the first of two"
        );
    }

    #[test]
    fn with_no_user_space_known_the_first_listed_is_home() {
        assert_eq!(
            home_space([(space(9), Some(false)), (space(4), None)]),
            Some(space(9))
        );
        assert_eq!(home_space([(space(9), None), (space(4), None)]), Some(space(9)));
        assert_eq!(home_space([]), None);
    }

    #[test]
    fn no_space_past_the_first_user_space_is_asked_about() {
        let mut asked = Vec::new();
        let spaces = [3, 4, 5].map(space);
        let home = home_space(spaces.iter().map(|&s| {
            asked.push(s);
            (s, Some(s == space(4)))
        }));
        assert_eq!(home, Some(space(4)));
        assert_eq!(asked, [space(3), space(4)]);
    }
}
