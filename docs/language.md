# The trendy language

A reference for the language as it exists so far. It is written for anyone
writing trendy code, language models included, and every example in it is
compiled by `crates/trendyc/tests/explanations.rs`: blocks marked `trendy`
must compile, and blocks marked `trendy,error` must not.

trendy is a relational language in the spirit of *The Third Manifesto*. Its
syntax is its own, and in particular it is not Tutorial D's.

## Programs

A source file is a sequence of top-level definitions, in any order: operator
definitions and variable declarations. Execution starts in the operator named
`main`, which every program must define.

```trendy
var greeting: text := "hello";

operator main{} [];
```

## Operators

```trendy
operator main{} [];
```

An operator definition is the keyword `operator`, a name, a parameter list in
braces `{}`, a body in square brackets `[]`, and a closing `;`. Parameters and
statements don't exist yet, so both are empty.

## Variables

```trendy
var count: integer := 0;
var limit: integer;
var name := "trendy";

operator main{} [];
```

A variable declaration is `var name: type := value;`. Either the type or the
value may be left out, but not both:

- Without a value, the variable starts at its type's default value.
- Without a type, the variable has the type of its value.

```trendy,error
var count;

operator main{} [];
```

Initial values are written with `:=`. As in Tutorial D, `=` means equality,
not assignment.

```trendy,error
var count: integer = 0;

operator main{} [];
```

## Types

| Type | Values | Default |
|---|---|---|
| `integer` | 64-bit signed integers | `0` |
| `rational` | Numbers with fractional parts, as 64-bit IEEE 754 binary floating point | `0.0` |
| `text` | Sequences of Unicode characters, stored as UTF-8 | `""` |

Type names are case-sensitive, and other languages' names such as `int`,
`double` or `string` are not types in trendy.

Every type has equality, and two values are equal exactly when no operator
can tell them apart, as *The Third Manifesto* requires (RM Prescription 8).
For `rational`, that means IEEE 754 totalOrder rather than the usual
floating-point comparison:

- two rationals are equal exactly when they are the same bits;
- `-0.0` and `0.0` are different values, because `1/x` tells them apart;
- every NaN is equal to itself.

Ordering and sorting use the same total order:
`-NaN < -∞ < … < -0.0 < 0.0 < … < ∞ < NaN`.

Values never convert to another type implicitly, as *The Third Manifesto*
requires. An integer literal can't initialize a `rational`: write `1.0`, not
`1`.

```trendy,error
var ratio: rational := 1;

operator main{} [];
```

## Literals

**Integer literals** are decimal digits, such as `0` or `42`, up to
`9223372036854775807`. There are no digit separators (`1_000`), no radix
prefixes (`0xFF`), and no negative literals yet, since there is no unary
minus.

**Rational literals** have digits on both sides of a decimal point, an
exponent, or both, such as `0.5`, `17.0`, `0e0` or `1.5e-3`. The exponent
starts with `e` or `E`, and its sign is optional. A bare leading or trailing
point (`.5`, `5.`) is not allowed, so each number has one spelling.
Magnitudes up to about `1.8e308` can be written.

```trendy
var zero: rational := 0.0;
var also_zero: rational := 0e0;
var small := 1.5e-3;
var large := 2.5E+10;

operator main{} [];
```

```trendy,error
var half: rational := .5;

operator main{} [];
```

**Text literals** are enclosed in double quotes, such as `"hello"`. A literal
must end on the line it starts. These escape sequences are available:

| Escape | Meaning |
|---|---|
| `\"` | quote |
| `\\` | backslash |
| `\n` | line feed |
| `\r` | carriage return |
| `\t` | tab |
| `\0` | null |
| `\u{1F600}` | the Unicode character with that code point, in 1 to 6 hex digits |

```trendy
var path: text := "C:\\data";
var poem: text := "two\nlines";
var smile: text := "\u{1F600}";

operator main{} [];
```

Any other backslash sequence is an error.

```trendy,error
var path: text := "C:\data";

operator main{} [];
```

## Names

Names are ASCII letters, digits and `_`, and don't start with a digit. They
are case-sensitive. The keywords are `operator` and `var`.

Variables and operators share one namespace, so no two of them can have the
same name. Types have their own namespace, so a variable can be named after a
type.

```trendy
var text: text := "allowed";

operator main{} [];
```

```trendy,error
var main: integer := 0;

operator main{} [];
```

## Layout

`trendyfmt` defines the canonical layout:

- one definition per line;
- consecutive variable declarations on adjacent lines;
- a blank line between other definitions;
- single spaces around `:=` and after `:`.

## Coming from other languages

| Elsewhere | In trendy |
|---|---|
| `fn`, `def`, `function`, `void main()` | `operator main{} [];` |
| `let`, `const`, `int x` | `var x` |
| `x = 0` | `x := 0` |
| `(params) { body }` | `{params} [body]` |
| `int`, `long`, `i64` | `integer` |
| `float`, `double`, `real`, `f64` | `rational` |
| `.5`, `5.` | `0.5`, `5.0` |
| `string`, `str`, `varchar` | `text` |

The compiler recognizes each of these habits and suggests the fix.
`trendyc --explain <code>` describes any error code in detail, and
`trendyc --error-format=json` prints each error as JSON, with its fixes as
exact edits.
