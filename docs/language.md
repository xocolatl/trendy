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
operator greet{name: text, times: integer} [
    call debug_value{v: name};
    call debug_value{v: times};
];

operator main{} [
    call greet{name: "Ada", times: 2};
];
```

An operator definition is the keyword `operator`, a name, its parameters, a
body in square brackets `[]`, and a closing `;`.

- **Parameters are a heading:** `{name: type, …}`, with each name unique and
  in any order. `main` has none, since nothing passes it arguments.
- **A body is a list of statements,** each ending with `;`. For now, the only
  statement is `call`.
- **Arguments are a tuple of the parameters:** `call greet{name: "Ada",
  times: 2};`. Every argument is named, every parameter gets one, and the
  order doesn't matter.

Following *The Third Manifesto*, operators are either read-only operators,
which return a value and are used in expressions, or update operators, which
return nothing and are invoked by a `call` statement. So far there are only
update operators.

```trendy,error
operator main{} [
    debug_value{v: 1};
];
```

```trendy,error
operator main{} [
    call debug_value{1};
];
```

### Names

Inside an operator, a name refers to one of its parameters, or else to a
variable of the program: parameters hide variables with the same name. An
operator is not a value, so its name can only follow `call`.

```trendy
var greeting := "hello";

operator show{greeting: integer} [
    call debug_value{v: greeting};
];

operator main{} [
    call debug_value{v: greeting};
    call show{greeting: 1};
];
```

For now, the attributes of a tuple or relation value in an operator's body
can't refer to parameters or variables: those values are built when
compiling. A variable's initial value can refer to other variables (see
[Variables](#variables)), so declare one and use it instead.

```trendy,error
operator show{n: integer} [
    call debug_value{v: tuple { n: n }};
];

operator main{} [];
```

### Built-in operators

The compiler implements some operators itself. They are declared in the
*prelude*, trendy source that is part of every program, so they are always in
scope. A program can't declare built-ins itself. The prelude also declares the
built-in types, and `any`: a type name reserved for the parameters of built-in
operators that accept a value of every type.

```
builtin type any;
builtin type boolean;
builtin type integer;
builtin type rational;
builtin type text;

builtin operator debug_value{v: any};
```

**`debug_value{v: any}`** writes `v` to stderr, on one line, as trendy source.
Tuples and relations are written with their attributes in name order,
relations with their heading first and their tuples in a fixed order, so the
output reads back as the same value. For example:
`relation { a: boolean, b: integer } { { a: false, b: 25 }, { a: true, b: 3 } }`.

`any` is a type name only, so a variable can be named `any`.

```trendy,error
operator show{v: any} [];

operator main{} [];
```

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

An initial value is any expression, and can refer to other variables,
declared before or after it. Initial values are evaluated when compiling,
each after the ones it refers to, so the program starts with every variable
already holding its value: below, `ready` is simply `true`.

```trendy
var ready := started or false;
var started := not stopped;
var stopped: boolean;

operator main{} [];
```

Variables whose initial values refer to each other in a cycle have no order
to be evaluated in, which is an error.

```trendy,error
var a := b;
var b := a;

operator main{} [];
```

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

## Expressions

### Boolean operators

| Operator | Result |
|---|---|
| `not a` | `true` when `a` is `false` |
| `a and b` | `true` when both are `true` |
| `a or b` | `true` when either is `true`, or both |
| `a xor b` | `true` when exactly one is `true` |
| `a implies b` | `false` only when `a` is `true` and `b` is `false`: the same as `not a or b` |

The operands must be booleans: there is no truthiness, so `not 0` and
`"" or true` are errors.

```trendy
var a := true;
var b := not a or a and false;
var c := (a or b) and not b;
var d := a implies b implies c;

operator check{p: boolean, q: boolean} [
    call debug_value{v: p xor q};
    call debug_value{v: not (p and q)};
];

operator main{} [
    call check{p: a, q: true};
];
```

### Integer operators

| Operator | Result |
|---|---|
| `-a` | `a` negated |
| `+a` | `a` |
| `a + b`, `a - b`, `a * b` | the sum, difference and product |
| `a div b` | the quotient, rounded toward zero: `-7 div 2` is `-3` |
| `a mod b` | the remainder of `div`, with the sign of `a`: `-7 mod 2` is `-1`, and `7 mod -2` is `1` |

`div` and `mod` always satisfy `a = (a div b) * b + a mod b`. The operands
must be integers: there are no implicit conversions, so `1 + 1.5` and
`1 + true` are errors.

An operator whose result doesn't fit in 64 bits has no result, and neither
does `div` or `mod` by zero. They never wrap around. When the operands are
known while compiling, this is a compile error (E0122 for overflow, E0123
for division by zero). Otherwise the program stops at that operator, writing
the error to stderr, such as `error[E0122]: integer overflow`, and exits
with status 1.

```trendy
var seven := 1 + 2 * 3;
var smallest := -9223372036854775808;
var largest := -(smallest + 1);

operator show{a: integer, b: integer} [
    call debug_value{v: -a + b * 2};
    call debug_value{v: a div b};
    call debug_value{v: a mod b};
];

operator main{} [
    call show{a: seven, b: -2};
];
```

```trendy,error
var big := 9223372036854775807 + 1;

operator main{} [];
```

```trendy,error
operator half{n: integer} [
    call debug_value{v: n div 0};
];

operator main{} [];
```

A `-` written directly before a number literal makes a negative literal, so
the smallest integer, `-9223372036854775808`, can be written, and so can
negative rationals such as `-1.5` and `-0.0`. Rationals have no other
operators yet.

### Precedence

From tightest to loosest:

| Operators | Grouping |
|---|---|
| prefix `-` and `+` | |
| `*`, `div`, `mod` | left |
| `+`, `-` | left |
| `not` | |
| `and` | left |
| `xor` | left |
| `or` | left |
| `implies` | right |

So `-a * b` is `(-a) * b`, and `not a + b` is `not (a + b)`.
`implies` groups from the right, as in logic: `a implies b implies c`
means `a implies (b implies c)`. Parentheses group explicitly.

### Other languages' operators

The symbols other languages use, such as `&&`, `||`, `!`, `^`, `->`, `/`
and `%`, are not operators in trendy. Neither are capitalized spellings
such as `AND` or `MOD`. The compiler suggests the keyword for each.

```trendy,error
var a := true && false;
var b := 7 / 2 % 3;

operator main{} [];
```

### Evaluation

Expressions have no side effects. Every operand is evaluated, so an error
in one is never skipped. Whatever doesn't depend on parameters or variables
is evaluated when compiling, and so is whatever a constant operand decides,
when the other operand can't fail: `p and false` is just `false`,
`true and p` is just `p`, and `n * 1` is just `n`. But `(n + 1) * 0` is
still computed, since `n + 1` can overflow.

## Literals

**Boolean literals** are `true` and `false`, in lowercase.

```trendy
var done: boolean := false;
var ready := true;

operator main{} [];
```

**Integer literals** are decimal digits, such as `0` or `42`, up to
`9223372036854775807`, or negative with a `-` before them, down to
`-9223372036854775808`. There are no digit separators (`1_000`) and no
radix prefixes (`0xFF`).

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
are case-sensitive. The keywords are `operator`, `var`, `call`, `builtin`,
`tuple`, `relation`, `true`, `false`, `not`, `and`, `or`, `xor`,
`implies`, `div` and `mod`, all lowercase. `type` is a keyword
only after `builtin`.

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
  indented four spaces per level of nesting;
- a relation's tuples list their attributes in the order of the relation's
  heading: the selector's own heading, else the declared type, else the first
  tuple's order;
- an operator's body has each statement on its own line, indented;
- parameters and arguments are written without spaces inside their braces:
  `operator greet{name: text}`, `call greet{name: "Ada"}`;
- single spaces around dyadic operators and after `not`, none after prefix
  `-` and `+`, and none inside parentheses: `not (a and b)`,
  `-a * (b + 1)`.

A trailing comma is kept or left out as written.

## Coming from other languages

| Elsewhere | In trendy |
|---|---|
| `fn`, `def`, `function`, `void main()` | `operator main{} [];` |
| `print(x)`, `f(x)` | `call debug_value{v: x};` |
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
| `!a`, `a && b`, `a \|\| b` | `not a`, `a and b`, `a or b` |
| `a ^ b`, `a != b` for booleans | `a xor b` |
| `a -> b`, `a => b` | `a implies b` |
| `AND`, `Or`, `NOT` | `and`, `or`, `not` |
| `a / b`, `a % b` for integers | `a div b`, `a mod b` |

The compiler recognizes each of these habits and suggests the fix.
`trendyc --explain <code>` describes any error code in detail, and
`trendyc --error-format=json` prints each error as JSON, with its fixes as
exact edits.
