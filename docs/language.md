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
| `boolean` | `true` and `false` | `false` |
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

## Tuples

A tuple type is written `tuple { name: type, … }`, and a tuple value
`tuple { name: value, … }`. Tuples are values like any other: an attribute
can itself be a tuple, and a variable can hold one.

```trendy
var point: tuple { x: rational, y: rational } := tuple { x: 1.0, y: 2.0 };
var person := tuple { name: "Ada", born: tuple { year: 1815, month: 12 } };
var unit: tuple {};

operator main{} [];
```

- **Attributes are unordered.** A tuple's attributes are a set, so
  `tuple { x: rational, y: rational }` and `tuple { y: rational, x: rational }`
  are the same type, and a value can list its attributes in any order.
- **Types are structural.** Two tuple types with the same attribute names and
  types are the same type, wherever they are written.
- **Defaults are attribute-wise.** A tuple variable declared without a value
  starts with every attribute at its own default. So
  `tuple { a: tuple { b: boolean }, c: text }` starts as
  `tuple { a: tuple { b: false }, c: "" }`.
- **Equality is attribute-wise.** Two tuples are equal when every attribute's
  values are equal.
- **Every attribute is required.** A value must have exactly the attributes
  of its declared type, each with a distinct name.
- `tuple {}`, the tuple with no attributes, is a valid type with exactly one
  value.
- A trailing comma after the last attribute is allowed.

```trendy
var reordered: tuple { x: integer, y: integer } := tuple { y: 2, x: 1, };

operator main{} [];
```

```trendy,error
var point: tuple { x: rational, y: rational } := tuple { x: 1.0 };

operator main{} [];
```

## Relations

A relation type is written `relation { name: type, … }`. A relation value is
a set of tuples that all have the relation's heading. It's written
`relation { tuple, … }`, and inside a relation the `tuple` keyword may be
left out, since a relation can hold nothing else.

```trendy
var empty: relation { a: boolean, b: integer };
var people := relation {
    tuple { name: "Ada", born: 1815 },
    { name: "Edgar", born: 1923 },
};

operator main{} [];
```

A relation selector can start with its heading, and so can a tuple
selector, written before the values: `relation { name: type, … } { … }` and
`tuple { name: type, … } { … }`. The heading is checked against the tuples.

```trendy
var r := relation { a: integer } { { a: 1 } };
var t := tuple { a: integer } { a: 1 };

operator main{} [];
```

- **The heading comes from context.** It's taken from the selector's own
  heading, from the type expected of the value (such as the variable's
  declared type), or from the first tuple, in that order. So an empty
  relation needs one of the first two.
- **The default is the empty relation.** A relation variable declared without
  a value starts with no tuples.
- **No duplicates.** A relation is a set, so listing the same tuple twice is
  an error, even with its attributes in another order.
- **Equality is set equality.** Two relations are equal when they have the
  same heading and the same tuples. Tuples have no order.
- **Relations are values like any other.** A tuple's attribute can be a
  relation, and so can a relation's.

```trendy
var typed: relation { name: text } := relation {};
var friends: tuple { name: text, friends: relation { name: text } };

operator main{} [];
```

```trendy,error
var unknown := relation {};

operator main{} [];
```

```trendy,error
var twice := relation { { a: 1 }, { a: 1 } };

operator main{} [];
```

## Literals

**Boolean literals** are `true` and `false`, in lowercase.

```trendy
var done: boolean := false;
var ready := true;

operator main{} [];
```

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
are case-sensitive. The keywords are `operator`, `var`, `tuple`, `relation`,
`true` and `false`, all lowercase.

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
- consecutive variable declarations on adjacent lines, unless you separate
  them with a blank line to group them; several blank lines become one;
- a blank line between other definitions;
- single spaces around `:=` and after `:`;
- a relation value with more than one tuple has each tuple on its own line,
  indented four spaces per level of nesting.

A trailing comma is kept or left out as written.

## Coming from other languages

| Elsewhere | In trendy |
|---|---|
| `fn`, `def`, `function`, `void main()` | `operator main{} [];` |
| `let`, `const`, `int x` | `var x` |
| `x = 0` | `x := 0` |
| `(params) { body }` | `{params} [body]` |
| `bool` | `boolean` |
| `True`, `TRUE`, `1` for true | `true` |
| `int`, `long`, `i64` | `integer` |
| `float`, `double`, `real`, `f64` | `rational` |
| `.5`, `5.` | `0.5`, `5.0` |
| `string`, `str`, `varchar` | `text` |
| `{ x: 1 }`, `TUPLE { X 1 }` | `tuple { x: 1 }` |
| `tuple { x = 1 }` | `tuple { x: 1 }` |
| `RELATION { TUPLE { … } }` | `relation { { … } }` |
| `VAR`, `TUPLE`, `TRUE` | `var`, `tuple`, `true` |

The compiler recognizes each of these habits and suggests the fix.
`trendyc --explain <code>` describes any error code in detail, and
`trendyc --error-format=json` prints each error as JSON, with its fixes as
exact edits.
