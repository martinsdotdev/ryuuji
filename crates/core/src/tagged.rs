//! One declaration per tagged enum, with its table derived from it.

/// Declares an enum whose variants each carry a stable `tag` and a
/// human-readable `label`, and derives `ALL`, `tag`, `from_tag` and `label`
/// from the one variant list.
///
/// `ALL` is the reason this exists. Written by hand its length is a number
/// the compiler never checks against the variant list, so a variant missed
/// there compiles and only fails later at runtime. It stays a fixed-size
/// array rather than a slice: callers need `[T; N]::map`, and pass `&ALL`
/// where a `&'static [T]` is wanted and rely on the unsize coercion.
///
/// The generated round-trip test is named after the enum, so it reports as
/// `test Page ... ok`. Sharing the list cannot catch two variants spelling
/// the same tag, which would make `from_tag` answer with the earlier one;
/// that test can.
macro_rules! tagged_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$variant_meta:meta])* $variant:ident => $tag:literal, $label:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis enum $name {
            $( $(#[$variant_meta])* $variant, )+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: [Self; [$(Self::$variant),*].len()] = [$(Self::$variant),*];

            /// Stable identifier, as documented on the type.
            pub fn tag(self) -> &'static str {
                match self {
                    $( Self::$variant => $tag, )+
                }
            }

            /// Inverse of [`Self::tag`].
            pub fn from_tag(tag: &str) -> Option<Self> {
                Self::ALL.into_iter().find(|value| value.tag() == tag)
            }

            /// Human-readable label.
            pub fn label(self) -> &'static str {
                match self {
                    $( Self::$variant => $label, )+
                }
            }
        }

        #[cfg(test)]
        #[test]
        #[allow(non_snake_case)]
        fn $name() {
            for value in $name::ALL {
                assert_eq!($name::from_tag(value.tag()), Some(value));
            }
            assert_eq!($name::from_tag("nope"), None);
        }
    };
}

pub(crate) use tagged_enum;
