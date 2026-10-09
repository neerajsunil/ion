/// What a piece of code is, for coloring. Deliberately coarse: themes map each
/// kind to one style, and grammar-specific capture names are folded into these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Highlight {
    Keyword,
    Function,
    Type,
    String,
    Escape,
    Number,
    Constant,
    Comment,
    Property,
    Operator,
    Punctuation,
    Tag,
    Attribute,
    Label,
    Heading,
    Link,
    Emphasis,
}

impl Highlight {
    /// Maps a query capture name such as `function.method.builtin` to a kind.
    /// Returns `None` for captures drawn in the default text color.
    pub fn from_capture_name(name: &str) -> Option<Self> {
        let exact = match name {
            "string.escape" | "escape" | "string.special.symbol" => Some(Self::Escape),
            "variable.builtin" | "constant.builtin" | "boolean" => Some(Self::Constant),
            "variable.member" | "variable.other.member" => Some(Self::Property),
            "constructor" | "namespace" | "module" => Some(Self::Type),
            "text.title" | "markup.heading" => Some(Self::Heading),
            "text.literal" | "markup.raw" => Some(Self::String),
            "text.uri" | "text.reference" | "markup.link" | "markup.link.url" => Some(Self::Link),
            "text.emphasis" | "text.strong" | "markup.italic" | "markup.bold" => {
                Some(Self::Emphasis)
            }
            _ => None,
        };
        if exact.is_some() {
            return exact;
        }
        let root = name.split('.').next().unwrap_or(name);
        match root {
            "keyword" | "conditional" | "repeat" | "include" | "exception" | "storageclass" => {
                Some(Self::Keyword)
            }
            "function" | "method" => Some(Self::Function),
            "type" => Some(Self::Type),
            "string" | "character" => Some(Self::String),
            "number" | "float" => Some(Self::Number),
            "constant" => Some(Self::Constant),
            "comment" => Some(Self::Comment),
            "property" | "field" => Some(Self::Property),
            "operator" => Some(Self::Operator),
            "punctuation" | "delimiter" => Some(Self::Punctuation),
            "tag" => Some(Self::Tag),
            "attribute" => Some(Self::Attribute),
            "label" => Some(Self::Label),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Highlight;

    #[test]
    fn folds_capture_names() {
        assert_eq!(
            Highlight::from_capture_name("keyword.control.return"),
            Some(Highlight::Keyword)
        );
        assert_eq!(
            Highlight::from_capture_name("function.method"),
            Some(Highlight::Function)
        );
        assert_eq!(
            Highlight::from_capture_name("string.escape"),
            Some(Highlight::Escape)
        );
        assert_eq!(Highlight::from_capture_name("variable"), None);
        assert_eq!(Highlight::from_capture_name("embedded"), None);
    }
}
