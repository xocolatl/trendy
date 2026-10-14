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
operator greet{name: text, count: integer} [
    call debug_value{v: name};
    call debug_value{v: count};
];

operator main{} [
    call greet{name: "Ada", count: 2};
];
```

An operator definition is the keyword `operator`, a name, its parameters, a
body in square brackets `[]`, and a closing `;`.

- **Parameters are a heading:** `{name: type, …}`, with each name unique and
  in any order. `main` has none, since nothing passes it arguments.
- **A body is a list of statements,** each ending with `;`. For now, the only
  statement is `call`.
- **Arguments are a tuple of the parameters:** `call greet{name: "Ada",
  count: 2};`. Every argument is named, every parameter gets one, and the
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

A tuple or relation value's attributes can be any expression, names
included, as in `tuple { n: n }`; see [Tuples](#tuples).

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
builtin type approximate;
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

A relation variable can also declare keys, after its value or type; see
[Keys](#keys).

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
| `approximate` | Numbers with fractional parts, as 64-bit IEEE 754 binary floating point | `0e0` |
| `text` | Sequences of Unicode characters, stored as UTF-8 | `""` |

Type names are case-sensitive, and other languages' names such as `int`,
`double` or `string` are not types in trendy.

Every type has equality, and two values are equal exactly when no operator
can tell them apart, as *The Third Manifesto* requires (RM Prescription 8).
For `approximate`, that means IEEE 754 totalOrder rather than the usual
floating-point comparison:

- two approximate numbers are equal exactly when they are the same bits;
- `-0e0` and `0e0` are different values, because `1/x` tells them apart;
- every NaN is equal to itself.

Ordering and sorting use the same total order:
`-NaN < -∞ < … < -0e0 < 0e0 < … < ∞ < NaN`.

Values never convert to another type implicitly, as *The Third Manifesto*
requires. An integer literal can't initialize an `approximate`: write `1e0`,
not `1`.

```trendy,error
var ratio: approximate := 1;

operator main{} [];
```

## Tuples

A tuple type is written `tuple { name: type, … }`, and a tuple value
`tuple { name: value, … }`. Tuples are values like any other: an attribute
can itself be a tuple, and a variable can hold one.

```trendy
var point: tuple { x: approximate, y: approximate } := tuple { x: 1e0, y: 2e0 };
var person := tuple { name: "Ada", born: tuple { year: 1815, month: 12 } };
var unit: tuple {};

operator main{} [];
```

- **Attributes are unordered.** A tuple's attributes are a set, so
  `tuple { x: approximate, y: approximate }` and
  `tuple { y: approximate, x: approximate }` are the same type, and a value
  can list its attributes in any order.
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
- **Attribute values can be any expression,** including parameters,
  variables and other operators: `tuple { street: name || "!", n: n + 1 }`.
  A tuple whose values are all known when compiling is built then; others
  are built when the program runs.

```trendy
var reordered: tuple { x: integer, y: integer } := tuple { y: 2, x: 1, };

operator main{} [];
```

```trendy,error
var point: tuple { x: approximate, y: approximate } := tuple { x: 1e0 };

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
- **Tuples that turn out equal become one.** A relation's tuples can be
  built from values known only when the program runs, so
  `relation { { x: a }, { x: b } }` has one tuple when `a = b`, and two
  otherwise. Tuples known when compiling must still be distinct.
- **Any tuple can be an element,** not only a tuple selector: a variable,
  a parameter, or any expression of a tuple, as in
  `relation { ada, { name: "Edgar", born: 1923 } }`. Each must have the
  relation's heading; one that isn't a tuple is an error (A0004).
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

## Keys

A relation variable can declare keys, each written `key { … }` after its
value, or after its type when it has no value. A key is a set of the
relation's attributes that no two of its tuples have the same values for: a
country's name, a language's code. *The Third Manifesto* calls these
candidate keys, and requires every relation variable to have at least one
(RM Prescription 15).

```trendy
var countries := relation {
    { name: "Kenya", code: "KE", capital: "Nairobi" },
    { name: "Peru", code: "PE", capital: "Lima" },
}
key { name }
key { code };

var visits: relation { country: text, year: integer, days: integer } key { country, year };

operator main{} [];
```

- **Keys can share attributes.** `key { day, gate } key { day, pilot }`
  says that a gate has one flight per day, and so does a pilot.
- **No key contains another.** A key is *irreducible*: no smaller set of its
  attributes tells the tuples apart (Date and Darwen, chapter 2). If
  `{ code }` does, `{ code, name }` isn't a key, so declaring both is an
  error (A0025), and so is declaring one key twice.
- **No key is primary.** Every key is a candidate key, and none is more
  important than the others. SQL's `PRIMARY KEY` and `UNIQUE` are both
  written `key` (P0019).
- **A variable without a key has one anyway:** all of its attributes, since a
  relation never has two equal tuples.
- **`key {}` allows at most one tuple,** since every tuple has the same
  values for no attributes.
- **Only relation variables have keys,** not tuple or scalar variables
  (A0020), and not types or values: a key is a rule about what a variable
  can hold.
- **An initial value must satisfy the keys.** It's checked when compiling,
  and two tuples with the same values for a key are an error (A0026),
  reported at the two tuples.
- `key` is a keyword only there, so it can still be a name: an attribute or
  a variable can be called `key`.

```trendy,error
var capitals := relation {
    { country: "Kenya", city: "Nairobi" },
    { country: "Kenya", city: "Mombasa" },
}
key { country };

operator main{} [];
```

```trendy,error
var languages := relation { { code: "sw", name: "Kiswahili" } } key { code } key { code, name };

operator main{} [];
```

```trendy,error
var rivers: relation { name: text } key { name } := relation { { name: "Mekong" } };

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

### Arithmetic

| Operator | Operands | Result |
|---|---|---|
| `-a` | integer or approximate | `a` negated |
| `+a` | integer or approximate | `a` |
| `a + b`, `a - b`, `a * b` | two integers or two approximate numbers | the sum, difference and product |
| `a / b` | approximate numbers | the quotient |
| `a div b` | integers | the quotient, rounded toward zero: `-7 div 2` is `-3` |
| `a mod b` | integers | the remainder of `div`, with the sign of `a`: `-7 mod 2` is `-1`, and `7 mod -2` is `1` |

`div` and `mod` always satisfy `a = (a div b) * b + a mod b`.

Values never convert to another type implicitly, so both operands have the
same type: `1 + 1.5e0` and `1 + true` are errors. The type comes from the
context when it says, as in `var r: approximate := 1 + 2;`, where `1` and `2`
must be written `1e0` and `2e0`, and otherwise from the left operand. `/`
doesn't divide integers, and `div` doesn't divide approximate numbers (A0022).

```trendy
var seven := 1 + 2 * 3;
var smallest := -9223372036854775808;
var largest := -(smallest + 1);
var half := 1e0 / 2e0;

operator show{a: integer, b: integer} [
    call debug_value{v: -a + b * 2};
    call debug_value{v: a div b};
    call debug_value{v: a mod b};
];

operator main{} [
    call show{a: seven, b: -2};
    call debug_value{v: half * 3e0};
];
```

```trendy,error
var half := 7 / 2;

operator main{} [];
```

Approximate arithmetic is IEEE 754 binary64 arithmetic, rounded to nearest:
`0.1e0 + 0.2e0` is `0.30000000000000004e0`, and signed zeros follow its rules, so
`0e0 * -1e0` is `-0e0`.

An operator whose result is out of range has no result: an integer outside 64
bits, or an approximate number beyond about `1.8e308` in magnitude. Neither
does division by zero, including by `-0e0`. Results never wrap around and
never become infinities or NaN, so every approximate number is finite and
can be written as a literal. When the operands are known while compiling,
this is a compile error (R0001 for overflow, R0002 for division by zero).
Otherwise the program stops at that operator, writing the error to stderr,
such as `error[R0001]: integer overflow`, and exits with status 1.

```trendy,error
var big := 9223372036854775807 + 1;

operator main{} [];
```

```trendy,error
operator inverse{x: approximate} [
    call debug_value{v: x / 0e0};
];

operator main{} [];
```

A `-` written directly before a number literal makes a negative literal, so
the smallest integer, `-9223372036854775808`, can be written, and so can
`-0e0`.

### Text

`a || b` is the text `a` followed by the text `b`. Both operands must be
text: `"n = " || 1` is an error, since values never convert implicitly, and
`+` doesn't concatenate (A0022, with a fix to `||`).

```trendy
var greeting := "hello" || ", " || "world";

operator greet{name: text} [
    call debug_value{v: "hello, " || name || "!"};
];

operator main{} [
    call greet{name: "Ada"};
];
```

```trendy,error
var greeting := "hello" + "world";

operator main{} [];
```

Concatenations of constants are done when compiling. One done while the
program runs allocates memory for its result, and stops the program with
`error[R0003]: out of memory` if there is none.

### Comparisons

`a = b`, `a <> b`, `a < b`, `a <= b`, `a > b` and `a >= b` compare two
values of the same type, and are booleans. Every type has `=` and `<>`.
The built-in types are also ordered:

| Type | Order |
|---|---|
| `boolean` | `false` before `true` |
| `integer` | numerical |
| `approximate` | IEEE 754 totalOrder (see [Types](#types)): numerical, except that `-0e0` is before `0e0`, and they aren't equal |
| `text` | by code point, which is also the order of the UTF-8 bytes. There is no collation: `"Z" < "a"`, and `"é" > "z"` |

Two values are equal exactly when they are the same value, as *The Third
Manifesto* requires (RM Prescription 8). Two tuples are equal when every
attribute's values are equal, and two relations when they have the same
tuples, in any order; the values inside are compared the same way, so
`tuple { x: 0e0 } <> tuple { x: -0e0 }`. Tuples and relations have no
order, so `<`, `<=`, `>` and `>=` don't apply to them (A0022).

The right operand of a tuple or relation comparison takes its heading
from the left one, so `r = relation {}` asks whether `r` is empty.

```trendy
var sorted := "apple" < "banana" and -1 < 0 and false < true;

operator check{n: integer, name: text} [
    call debug_value{v: n >= 0 and n <= 100};
    call debug_value{v: name <> ""};
];

operator same{a: tuple { x: integer }, r: relation { x: integer }} [
    call debug_value{v: a = tuple { x: 1 }};
    call debug_value{v: r = relation {}};
];

operator main{} [
    call check{n: 42, name: "Ada"};
    call same{a: tuple { x: 1 }, r: relation { { x: 2 }, { x: 1 } }};
];
```

```trendy,error
var a := tuple { x: 1 } < tuple { x: 2 };

operator main{} [];
```

The operands must have the same type, since values never convert
implicitly: `1 = 1e0` is an error. Comparisons don't chain: `a < b < c` is
an error, with a fix to `a < b and b < c` (P0015). To compare two
comparisons, parenthesize them: `(a < b) = (c < d)`.

```trendy,error
var a := 1 < 2 < 3;

operator main{} [];
```

Equality is `=`, and inequality `<>`, as in Tutorial D and SQL; `==` and
`!=` get fixes. Assignment is `:=`.

### Attribute values

`t.a` is the value of the tuple `t`'s attribute `a`, and `t.a.b` reaches
into a tuple-valued attribute. It's the *Manifesto*'s attribute extractor
(RM Prescription 6), written with the dot Date and Darwen suggest.

```trendy
var addr := tuple { city: "Paris", place: tuple { postal_code: "75001" } };
var city := addr.city;
var code := addr.place.postal_code;

operator main{} [
    call debug_value{v: "FR-" || addr.place.postal_code};
];
```

- **Only tuples have attribute values.** A relation can have many tuples,
  so `r.a` is an error (A0022); to keep only the attribute, project on it:
  `r{a}`.
- **An unknown attribute** gets a fix to the closest one (A0010).
- It binds tighter than any other operator, with projection, `remove` and
  `rename`, so `-t.n` negates `t.n`, and `(t rename {a as b}).b` is `t.a`.

```trendy,error
var addr := tuple { city: "Paris" };
var town := addr.cty;

operator main{} [];
```

### Projection

`r{a, b}` is the tuple or relation `r` with only the attributes
`a` and `b`, and `r remove {a, b}` is `r` without them. They are the
tuple and relational operators of *The Third Manifesto*'s projection, and
`remove` is Algebra A's REMOVE (Appendix A), of which projection is the
complement. A projection is written like Tutorial D's, with the names in
braces right after the operand.

```trendy
var people := relation {
    { name: "Ada", born: 1815 },
    { name: "Ada", born: 1816 },
};
var names := people{name};
var years := people remove {name};
var ada := tuple { name: "Ada", born: 1815 } remove {born};

operator main{} [
    call debug_value{v: names};
];
```

- **The result is a set.** Projecting a relation can make tuples equal,
  and duplicates are dropped: `names` above has one tuple.
- **The names must be the operand's attributes,** each named once. An
  unknown name gets a fix renaming it to the closest attribute.
- **`r{}` keeps nothing:** a tuple becomes `tuple {}`, and a relation
  becomes `relation {} { {} }` (table_dee) if it has tuples, or
  `relation {} {}` (table_dum) if not. `remove {}` removes nothing, and is
  an error (A0023).
- **They bind tighter than any other operator,** so `-t remove {x}`
  negates the result. Chained, they apply left to right:
  `r{a, b} remove {b}`.

```trendy,error
var ada := tuple { name: "Ada", born: 1815 };
var name := ada{nmae};

operator main{} [];
```

### Renaming

`r rename {postal_code as code, city as town}` is the tuple or relation `r`
with its attributes renamed, each written `old as new`. It is Algebra A's
RENAME.

```trendy
var addr := tuple { postal_code: "75001", city: "Paris" };
var moved := addr rename {postal_code as code};
var swapped := addr rename {postal_code as city, city as postal_code};

operator main{} [
    call debug_value{v: moved};
];
```

- **All the renamings happen at once,** so attributes can swap names, as
  in `swapped` above.
- **The old names must be the operand's attributes,** each renamed once.
  A renaming written the wrong way around, whose new name is the
  attribute, gets a fix that swaps it.
- **The new names must be free:** no attribute that keeps its name, and no
  other renaming, can have it (A0024).
- **`rename {}` renames nothing,** and is an error (A0023).
- `old: new`, as in mappings elsewhere, is fixed to `old as new` (P0018).

```trendy,error
var addr := tuple { postal_code: "75001", city: "Paris" };
var moved := addr rename {code as postal_code};

operator main{} [];
```

### Names and values: `:`, `:=` and `as`

Three separators pair a name with something, each with one meaning:

- **`name: value`** pairs a name with what is given: a tuple's attribute
  with its value, a heading's with its type, a parameter with its
  argument.
- **`name := …`** defines a new name by what follows: a variable by its
  initial value, an attribute by the value `extend` or `replace` computes
  for each tuple, or by the attributes `group` or `wrap` gathers. The new
  name comes first.
- **`old as new`** gives something that exists another name, in `rename`.
  The new name comes last, and nothing is computed.

Tutorial D writes `AS` for all three kinds of new attribute, the value
first; SQL does the same. trendy fixes those habits to `:=` (P0011).

### Extension

`r extend { total := price * quantity }` is the tuple, or each tuple of the
relation, `r` with new attributes added, each written `name := value`. The
value is computed from the tuple's own attributes, which are in scope by
name. It is *The Third Manifesto*'s EXTEND, written as the definition it is:
`:=` defines a new attribute, as it defines a variable.

```trendy
var items := relation {
    { name: "tea", price: 3, quantity: 4 },
    { name: "rice", price: 2, quantity: 10 },
};
var totals := items extend { total := price * quantity, label := name || "!" };
var point := tuple { x: 1, y: 2 } extend { sum := x + y };

operator main{} [
    call debug_value{v: totals};
];
```

- **The tuple's attributes hide other names.** Inside the values, `price`
  is the attribute, even if a variable or parameter is named `price` too;
  to use one of those, rename the attribute first. In a nested `extend`,
  the inner tuple's attributes hide the outer's.
- **The values see only the operand's attributes,** not each other's, so
  their order doesn't matter: `extend { a := 1, b := a }` is an error
  unless the operand has an `a`. To use one in the next, chain them:
  `extend { a := 1 } extend { b := a }`. Brackets, `extend [a := 1, b := a]`,
  would ask for that order, and are fixed to the chain (P0003).
- **New names must be new.** An attribute the operand already has is an
  error (A0024): to give it a new value, use [`replace`](#substitution).
  A name given twice is an error (A0008), and so is `extend {}`, which
  adds nothing (A0023).
- **The values are computed for each tuple** when the program runs, or when
  compiling if the operand is known then. An empty relation has no tuple
  to compute them for, but the result still has the new attributes.
- `name: value`, as in a tuple, and `name = value` are fixed to
  `name := value`, and `value as name`, as SQL and Tutorial D's
  `EXTEND … ADD` write it, to `name := value` (P0011).

```trendy,error
var items := relation { { name: "tea", price: 3, quantity: 4 } };
var totals := items extend { total: price * quantity };

operator main{} [];
```

### Substitution

`r replace { price := price * 2 }` is the tuple, or each tuple of the
relation, `r` with attributes it has given new values, each written
`name := value`. The values are computed from the tuple's own attributes,
in scope by name as in `extend`. It is Tutorial D's read-only `UPDATE`
(*The Third Manifesto*, chapter 5), which is shorthand for `extend` under
new names, `remove` of the old ones, and `rename` of the new.

```trendy
var items := relation {
    { name: "tea", price: 3 },
    { name: "rice", price: 2 },
};
var doubled := items replace { price := price * 2 };
var point := tuple { x: 1, y: 2 } replace { x := y, y := x };

operator main{} [
    call debug_value{v: doubled};
    call debug_value{v: point};
];
```

- **The values are computed from the old tuple, all at once,** as RM
  Prescription 21 has multiple assignment do: `replace { x := y, y := x }`
  swaps `x` and `y`. One after the other is a chain:
  `replace { x := y } replace { y := x }` makes both `y`. Brackets are
  fixed to the chain (P0003).
- **The attributes must exist, and keep their types.** An attribute the
  operand hasn't is an error (A0010): to add one, use `extend`. A value of
  another type is an error (A0004): to change an attribute's type, compute
  it under another name with `extend`, then `remove` the old one and
  `rename` the new. A name given twice is an error (A0008), and so is
  `replace {}`, which replaces nothing (A0023).
- **Tuples that become equal are one,** since a relation is a set: the
  result can have fewer tuples than the operand.
- Assignments are written as in `extend`, and the same habits are fixed
  (P0011). `replace` is not an update: it changes no variable, and leaves
  the operand as it was.

```trendy,error
var items := relation { { name: "tea", price: 3 } };
var doubled := items replace { price := price * 2e0 };

operator main{} [];
```

### Image relations

In the values of an `extend` or a `replace`, `!!r` is the image
relation of the tuple at hand in the relation `r`: the tuples of `r` that
match it on their common attributes, without those attributes. A tuple of
departments, say, has as its image in a relation of employees the
department's employees. It is Tutorial D's image relation, with its
syntax, from Date and Darwen's *Database Explorations*; it is the one
relational operator written with symbols rather than a word, as the
literature writes it.

```trendy
var cities := relation {
    { city: "Kumasi", country: "Ghana" },
    { city: "Accra", country: "Ghana" },
    { city: "Osaka", country: "Japan" },
};
var countries := relation { { country: "Ghana" }, { country: "Japan" }, { country: "Peru" } };
var by_country := countries extend { cities := !!cities };
var listed := countries extend { has_cities := !!(cities{country}) = relation { {} } };

operator main{} [
    call debug_value{v: by_country};
];
```

- **It is a join, then a remove:** `r` joined with the tuple at hand,
  projected on their common attributes, then without them. So, as in
  `join`, the relation must have an attribute in common with the tuple at
  hand (A0028), with the same type (A0027).
- **A tuple with no match has an empty image,** so every tuple at hand
  gets one: above, Peru's `cities` is empty, and so its `has_cities` is
  false, since its image in `cities{country}` isn't `relation { {} }`.
  This is grouping without SQL's GROUP BY, which loses the empty groups;
  with aggregate operators, it will also be how to summarize.
- **The tuple at hand is the innermost one:** in a nested `extend`, the
  inner tuple's. There is none outside an `extend`'s or a `replace`'s
  values, so `!!` there is an error (A0030).
- **Its operand is a single one:** a name, or an expression in
  parentheses. A postfix operator or a join right after it could apply
  to the image or to its operand, so it needs parentheses to say which
  (P0022): the image of `r{a}` is `!!(r{a})`, and the image
  projected is `(!!r){a}`.
- **`!!` is one token, the two `!`s together.** A lone `!` is C's `not`,
  fixed to `not` (P0014), so `! !r` is two of those, not an image.

```trendy,error
var births := relation { { name: "Ada", place: "London" } };
var places := relation { { place: "London" } };
var names := places extend { names := !!births{name, place} };

operator main{} [];
```

### Grouping

`r group { g := {a, b} }` gathers the attributes `a` and `b` into a
relation-valued attribute `g`: the result has one tuple for each value of
the attributes no grouping names, and in each, `g` is the `a` and `b` of
the tuples of `r` with that value. It is *The Third Manifesto*'s GROUP.

```trendy
var births := relation {
    { name: "Ada", born: 1815, place: "London" },
    { name: "Ada", born: 1816, place: "London" },
    { name: "Edgar", born: 1923, place: "Portland" },
};
var by_place := births group { births := {name, born} };
var names_years := births group { names := {name}, years := {born} };

operator main{} [
    call debug_value{v: by_place};
];
```

- **It names what's gathered,** not what's kept, the opposite of SQL's
  `GROUP BY`. SQL's `group by {…}` is reported (P0021), but can't be
  fixed mechanically, since the gathered attributes and the new name
  aren't written.
- **Several groupings are simultaneous:** each gathers from the same
  tuples, so their order doesn't matter, as the braces say. Above,
  London's `names` is Ada, and its `years` 1815 and 1816. Tutorial D
  applies its groupings in turn instead, where the order can change the
  result; in trendy, that is a chain, `r group { x := {a} } group { y := {b} }`,
  and brackets, `r group [x := {a}, y := {b}]`, are fixed to it (P0003).
  Each attribute goes into one grouping (A0008).
- **It is an extend with image relations:**
  `births group { births := {name, born} }` is
  `births{place} extend { births := !!births }`. Every group comes from a
  tuple of `r`, so none is empty; for groups that may be empty, such as
  places with no births, extend another relation with `!!`.
- **Names:** a new attribute's name can't be one the result keeps
  (A0024), but can be one it gathers, as `births` is above. Gathering
  every attribute gives one tuple, holding all of `r`, or none if `r` is
  empty. `g := {}` gathers nothing, so each tuple gets `relation { {} }`.
  `group {}` is an error (A0023), and so is `group` of a tuple (A0022).
- Tutorial D's `{a, b} as g`, a tuple's `g: {a, b}` and `g = {a, b}` are
  fixed to `g := {a, b}` (P0011).

```trendy,error
var births := relation { { name: "Ada", born: 1815, place: "London" } };
var by_place := births group ({name, born} as births);

operator main{} [];
```

### Ungrouping

`r ungroup {g}` replaces the relation-valued attribute `g` with its
attributes: each tuple of `r` becomes one tuple for each tuple of its `g`.
It is *The Third Manifesto*'s UNGROUP, and undoes `group`.

```trendy
var teams := relation {
    { team: "core", members: relation { { name: "Ada" }, { name: "Edgar" } } },
    { team: "docs", members: relation { name: text } {} },
};
var memberships := teams ungroup {members};

operator main{} [
    call debug_value{v: memberships};
];
```

- **A tuple whose relation is empty has no tuples:** above, the docs
  team isn't in the result. So grouping then ungrouping gives the
  relation back, but ungrouping then grouping loses the empty groups.
- **Several attributes at once** give each combination of a tuple of
  each: `r ungroup {g, h}`. The order doesn't matter, as the braces say.
- **Names must stay distinct:** an attribute of `g`'s tuples can't have
  the name of one the result keeps, or of another ungrouped relation's
  (A0024); the help says what to rename. Tuples that turn out equal are
  one.
- **Only relation-valued attributes,** of a relation (A0022): a
  tuple-valued attribute is unpacked by `unwrap`. `ungroup {}` is an
  error (A0023), and so is an unknown attribute (A0010).
- Tutorial D's `UNGROUP ( G )` is fixed to `ungroup {g}`, and so is a bare
  `ungroup g` (P0003).

```trendy,error
var teams := relation { { team: "core", members: relation { { name: "Ada" } } } };
var memberships := teams ungroup members;

operator main{} [];
```

### Wrapping

`t wrap { w := {a, b} }` gathers the attributes `a` and `b` of a tuple, or
of each tuple of a relation, into a tuple-valued attribute `w`, and
`t unwrap {w}` replaces it with its attributes again. They are *The Third
Manifesto*'s WRAP and UNWRAP (RM Prescriptions 6 and 7).

```trendy
var addr := tuple { street: "1 Rue de Rivoli", city: "Paris", country: "France" };
var placed := addr wrap { place := {street, city} };
var nested := placed wrap { location := {place, country} };
var flat := placed unwrap {place};

operator main{} [
    call debug_value{v: nested};
];
```

- **Written like `group`,** each wrapping `name := {attributes}`; a new
  name can be one it gathers, but not one the result keeps (A0024), and
  each attribute goes into one wrapping (A0008). Several wrappings are
  simultaneous; to wrap one into another, chain them, as `nested` does.
  Brackets, `wrap [v := {a}, w := {v, b}]`, are fixed to that chain
  (P0003).
- **`unwrap` is written like `ungroup`,** of tuple-valued attributes
  (A0022; a relation-valued one is `ungroup`'s). An attribute of the
  unwrapped tuple can't have the name of one the result keeps, or of
  another unwrapped tuple's (A0024).
- **Each undoes the other:** unwrapping what was wrapped gives the tuple
  back, and so does wrapping what was unwrapped. Both are an `extend`
  followed by a `remove`, as Appendix A defines them.
- Tutorial D's `WRAP ( {A, B} AS W )` is fixed to `wrap { w := {a, b} }`
  (P0011), and `UNWRAP ( W )` and a bare `unwrap w` to `unwrap {w}`
  (P0003).

```trendy,error
var addr := tuple { street: "1 Rue de Rivoli", city: "Paris" };
var placed := addr wrap ({street, city} as place);

operator main{} [];
```

### Restriction

`r where p` is the tuples of the relation `r` for which the condition `p`
is true. The condition is a boolean, computed for each tuple, with the
tuple's attributes in scope by name, as in `extend`'s values. It is
Tutorial D's `WHERE`.

```trendy
var cities := relation {
    { city: "Paris", country: "France", people: 2100000 },
    { city: "Kumasi", country: "Ghana", people: 3600000 },
    { city: "Lyon", country: "France", people: 520000 },
};
var french := cities where country = "France";
var big_french := cities where country = "France" and people > 1000000;

operator main{} [
    call debug_value{v: big_french};
];
```

- **`where` binds loosest:** its condition is all that follows, and what
  comes before it is restricted, so `r join s where p` restricts the join.
  Parenthesize a `where` to use its result in a larger expression.
- **The condition is a boolean** (A0004), of a relation's tuples: a tuple
  satisfies a condition or doesn't, so `where` takes relations (A0022).
- **The tuple's attributes hide other names,** as in `extend`, and
  `!!s` is its image relation in `s`.
- **Computed for each tuple** when the program runs, or when compiling if
  the relation is known then, where a failure names the tuple.
- Other languages' operators, such as `==` and `&&`, are fixed, as
  everywhere.

```trendy,error
var cities := relation { { city: "Paris", people: 2100000 } };
var big := cities where people;

operator main{} [];
```

### Taking the only tuple

`tuple from r` is the only tuple of the relation `r`, and `value from r`
is the only value of it, for a relation of one attribute. It is Tutorial
D's `TUPLE FROM R`. Typically, the relation is restricted first:

```trendy
var cities := relation {
    { city: "Paris", country: "France" },
    { city: "Kumasi", country: "Ghana" },
};
var paris := tuple from cities where city = "Paris";
var country := value from (cities where city = "Paris"){country};

operator main{} [
    call debug_value{v: country};
];
```

- **Exactly one tuple:** with none or several, it is an error, when
  compiling if the relation is known then, and otherwise when the program
  runs (R0005).
- **`value from` takes one attribute:** project on it, as in
  `value from r{country}`. A relation of several attributes is an error
  (A0022), with a fix for each attribute.
- **It takes a relation's expression,** as Tutorial D's does: the rest of
  it down to the joins, and a `where` with its condition, so
  `tuple from r where p` is of `r where p`. Postfix operators bind tighter,
  so `value from r{country}` takes from `r{country}`; to project after a
  `where`, parenthesize it, as above. A comparison binds more loosely, so
  `tuple from r = t` compares the tuple.
- **`value` and `from` are names elsewhere:** they are keywords only in
  `tuple from` and `value from`, as `key` is only where keys go.
  Tutorial D's capitals, `TUPLE FROM R`, are fixed.

```trendy,error
var people := relation { { name: "Ada" }, { name: "Edgar" } };
var somebody := value from people;

operator main{} [];
```

### Joins

`r join s` is the relation of the tuples made of a tuple of `r` and a tuple
of `s` that agree on their common attributes: those with the same name in
both. `r times s` pairs every tuple of `r` with every tuple of `s`, and
`r compose s` is `r join s` without the common attributes. All three are
Algebra A's AND (*The Third Manifesto*, Appendix A); compose is Algebra A's
COMPOSE. Each operator states what the operands have in common, so that an
attribute shared, or not shared, by mistake is caught:

- **`join` needs at least one common attribute,** which it matches on.
- **`times` needs none,** so every tuple pairs with every other.
- **`compose` needs at least one,** and the result doesn't have them.

```trendy
var cities := relation {
    { city: "Kumasi", country: "Ghana" },
    { city: "Osaka", country: "Japan" },
    { city: "Lima", country: "Peru" },
};
var countries := relation {
    { country: "Ghana", continent: "Africa" },
    { country: "Japan", continent: "Asia" },
};
var sizes := relation { { size: "small" }, { size: "large" } };
var placed := cities join countries;
var continents := cities compose countries;
var both := countries{continent} times sizes;

operator main{} [
    call debug_value{v: placed};
];
```

- **The result is a set,** with the attributes of both operands. A tuple
  that matches nothing, like Lima's above, isn't in the result.
- **Common attributes must have the same type** in both operands (A0027),
  since values never convert to another type implicitly.
- **Rename to match:** to match attributes with different names, or keep
  apart attributes with the same name, rename them first. `rename` binds
  tighter than a join, so `r join s rename {id as r_id}` renames `s`'s
  attribute.
- **Tuples join too:** `t join s` is the tuple of both tuples'
  attributes, with the same rules for common attributes, lists and
  `compose`; it is *The Third Manifesto*'s tuple union, also called tuple
  join. Tuples that disagree on a common attribute have no join: that is
  an error, when compiling if both are known then, and otherwise when the
  program runs (R0004). `times` of tuples never fails. A join takes
  relations, or tuples, but not both; a tuple among relations is fixed to
  a relation of it, `relation { t }`.
- **Lists:** `join{r, s, t}` and `times{r, s, t}` take any number of
  operands, in any order, since join and times are associative and
  commutative. Each operand of `join{…}` must have an attribute in common
  with another, so that they all connect, and no two operands of
  `times{…}` may share one. `join{}` and `times{}` are
  `relation {} { {} }` (table_dee), which joins with anything to give it
  back.
- **Compose's list is in brackets,** since compose isn't associative and
  the order matters: `compose[r, s, t]` is `(r compose s) compose t`. Each
  step needs an attribute in common between what comes before and the
  next operand. `compose[r]` is `r`, and `compose[]` has no value (A0029).
  Braces and brackets are fixed to the ones each operator takes.

```trendy
var cities := relation { { city: "Kumasi", country: "Ghana" } };
var countries := relation { { country: "Ghana", continent: "Africa" } };
var continents := relation { { continent: "Africa", area: 30370000 } };
var sizes := relation { { size: "small" }, { size: "large" } };
var placed := join{cities, countries, continents};
var everything := times{cities, sizes};
var areas := compose[cities, countries, continents];

operator main{} [
    call debug_value{v: placed};
];
```
```trendy
var ada := tuple { name: "Ada", born: 1815 };
var place := tuple { name: "Ada", city: "London" };
var joined := ada join place;
var dated := ada times tuple { year: 2024 };

operator main{} [
    call debug_value{v: joined};
];
```

```trendy,error
var ada := tuple { name: "Ada", born: 1815 };
var place := tuple { name: "Ada Lovelace", city: "London" };
var joined := ada join place;

operator main{} [];
```

- **SQL's spellings** are recognized (P0021): `natural join` and
  `inner join` are fixed to `join`, `cross join` to `times`, and `using`
  is deleted. `on` and the outer joins have no equivalent.

```trendy,error
var cities := relation { { city: "Kumasi", country: "Ghana" } };
var countries := relation { { county: "Ghana", continent: "Africa" } };
var placed := cities join countries;

operator main{} [];
```

### Precedence

From tightest to loosest:

| Operators | Grouping |
|---|---|
| `!!`, of a name or a parenthesized expression | none: see below |
| postfix `.a`, projection `{…}`, `remove {…}`, `rename {…}`, `extend {…}`, `replace {…}`, `group {…}`, `ungroup {…}`, `wrap {…}` and `unwrap {…}` | left |
| prefix `not`, `-`, `+`, `tuple from` and `value from` | |
| `*`, `/`, `div`, `mod` | left |
| `+`, `-`, `\|\|` | left |
| `join`, `times`, `compose` | left |
| `=`, `<>`, `<`, `<=`, `>`, `>=` | none: they don't chain |
| `and` | left |
| `xor` | left |
| `or` | left |
| `implies` | right |
| `where`, whose condition is all that follows | left |

So `-a * b` is `(-a) * b`, `a + 1 < b` is `(a + 1) < b`, and, as with
every prefix operator, `not a = b` is `(not a) = b`. To negate a
comparison, parenthesize it: `not (n = 0)`. Writing `not n = 0` for an
integer `n` is an error, since `not` takes a boolean, and its fix is that
parenthesization; SQL and Python read it that way.
`implies` groups from the right, as in logic: `a implies b implies c`
means `a implies (b implies c)`. The joins bind looser than arithmetic,
since their operands are relations, and tighter than the comparisons, so
`r join s = t` compares the join. Parentheses group explicitly.

Precedence is a partial order: where a pair of operators could reasonably
be read either way, neither reading is assumed, and parentheses must say
which is meant. These pairs are:

- **A comparison and another comparison,** since `a < b < c` doesn't
  chain (P0015).
- **`!!` and a postfix operator or a join after it:**
  `!!r rename {a as b}` could be the image of the renamed relation
  or the image renamed, and `!!r join s` could be the image of the
  join or the image joined. Write `!!(r rename {a as b})` or
  `(!!r) rename {a as b}` (P0022). Before it, there's no
  ambiguity: `s join !!r` joins `s` with the image.

```trendy,error
var births := relation { { name: "Ada", place: "London" } };
var cities := relation { { city: "London" } };
var natives := cities extend { natives := !!births rename {place as city} };

operator main{} [];
```

### Other languages' operators

The symbols other languages use, such as `&&`, `|`, `!`, `^`, `->`, `%`,
`==` and `!=`, are not operators in trendy. Neither are capitalized
spellings such as `AND` or `MOD`. The compiler suggests trendy's operator
for each. `||` is concatenation, as in Tutorial D and SQL, so `a || b` on
booleans is an error with a fix to `or`.

```trendy,error
var a := true && false;
var b := 7 % 3;
var c := a == b;

operator main{} [];
```

### Evaluation

Expressions have no side effects. Every operand is evaluated, so an error in
one is never skipped. Whatever doesn't depend on parameters or variables is
evaluated when compiling, and so is whatever a constant operand decides, when
the other operand can't fail: `p and false` is just `false`, `true and p` is
just `p`, and `n * 1` is just `n`. But `(n + 1) * 0` is still computed, since
`n + 1` can overflow. Approximate numbers are simplified only where IEEE 754
makes it exact, signed zeros included: `x * 1e0` is `x`, but `x + 0e0` isn't,
since `-0e0 + 0e0` is `0e0`.

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

**Approximate literals** always have an exponent, which starts with `e` or
`E` and has an optional sign, such as `1e0`, `0e0`, `2.5e0` or `1.5e-3`. A
decimal point is optional, with digits on both sides of it; a bare leading
or trailing point (`.5`, `5.`) is not allowed. A decimal number without an
exponent, such as `2.5`, is not an approximate literal: it is reserved for
exact numbers, which `approximate` isn't, since `1e-1` has no exact binary
representation. Magnitudes up to about `1.8e308` can be written. Values are
printed the same way, with the shortest exponent form that reads back as the
same value, so `0.5` prints as `5e-1`.

```trendy
var zero: approximate := 0e0;
var half: approximate := 5e-1;
var small := 1.5e-3;
var large := 2.5E+10;

operator main{} [];
```

```trendy,error
var half: approximate := 0.5;

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

## Comments

A comment is written `/* … */`, and can go anywhere whitespace can. There
are no line comments.

Comments nest: a `/*` inside a comment starts a nested comment, which needs
its own `*/`. So code that contains comments can be commented out as it is.

```trendy
/* The answer, which /* for now */ is a constant. */
var answer := 42; /* see below */

operator main{} [
    /* call debug_value{v: answer}; /* disabled */ */
];
```

A comment without its `*/` runs to the end of the file, and is an error
(P0012); so is a `*/` outside a comment (P0016).

```trendy,error
/* not closed /* */
var answer := 42;

operator main{} [];
```

## Names

Names are ASCII letters, digits and `_`, and don't start with a digit. They
are case-sensitive. The keywords are `operator`, `var`, `call`, `builtin`,
`tuple`, `relation`, `true`, `false`, `not`, `and`, `or`, `xor`,
`implies`, `div`, `mod`, `remove`, `rename`, `extend`, `replace`, `group`,
`ungroup`, `wrap`, `unwrap`, `join`, `times`, `compose`, `where` and `as`,
all lowercase, and a keyword can't be a name (P0017).
`type` is a keyword only after `builtin`, `key` only where a variable's
keys go, and `value` and `from` only in `tuple from` and `value from`.

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
- lines are at most 100 characters where a list can be broken: a line
  that would be longer has its outermost list, such as an operator's
  parameters, a call's arguments, a heading or a tuple value, written one
  item per line, until it fits;
- a relation value with more than one tuple has each tuple on its own line,
  indented four spaces per level of nesting;
- a tuple value, relation value or call's arguments with several items, one
  of which spans lines, has each item on its own line too, so nothing
  follows a value's last line but its closing braces; a single item stays
  inside its braces, as in `call f{v: relation {` … `}};`;
- a relation's tuples list their attributes in the order of the relation's
  heading: the selector's own heading, else the declared type, else the first
  tuple's order;
- an operator's body has each statement on its own line, indented, on
  adjacent lines unless you separate them with a blank line to group them;
  several blank lines become one;
- a variable's keys on the line before them, when they fit there, with
  spaces inside their braces: `var r: relation { a: integer } key { a };`.
  After a value that spans lines, or when they don't fit, each key goes on
  a line of its own, starting in the same column as the declaration, and
  so as a relation value's closing brace, and a blank line follows the
  last;
- parameters and arguments are written without spaces inside their braces:
  `operator greet{name: text}`, `call greet{name: "Ada"}`;
- single spaces around dyadic operators and after `not`, none after prefix
  `-` and `+`, and none inside parentheses: `not (a and b)`,
  `-a * (b + 1)`.

A trailing comma is kept or left out as written.

Comments are kept exactly as written. A comment on a line of its own stays
on its own line, indented like the code after it, where the layout breaks
the line anyway; elsewhere it stays in the line, between the same code. A
comment after code on the same line stays there, after one space. A blank
line after a comment on its own line, such as a file's header, is kept.

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
| `float`, `double`, `real`, `f64` | `approximate` |
| `0.5`, `.5`, `5.` | `0.5e0`, `5e0` |
| `string`, `str`, `varchar` | `text` |
| `{ x: 1 }`, `TUPLE { X 1 }` | `tuple { x: 1 }` |
| `tuple { x = 1 }` | `tuple { x: 1 }` |
| `RELATION { TUPLE { … } }` | `relation { { … } }` |
| `VAR`, `TUPLE`, `TRUE` | `var`, `tuple`, `true` |
| `!a`, `a && b` | `not a`, `a and b` |
| `a \|\| b` for booleans | `a or b` |
| `"a" + "b"`, `concat("a", "b")` | `"a" \|\| "b"` |
| `a ^ b`, `a != b` for booleans | `a xor b` |
| `a -> b`, `a => b` | `a implies b` |
| `AND`, `Or`, `NOT` | `and`, `or`, `not` |
| `a == b`, `a != b` | `a = b`, `a <> b` |
| `a < b < c` | `a < b and b < c` |
| `A FROM T`, `t["a"]` | `t.a` |
| `R { A, B }`, `R { ALL BUT C }` | `r{a, b}`, `r remove {c}` |
| `R RENAME ( A AS B )`, `rename(columns={'a': 'b'})` | `r rename {a as b}` |
| `EXTEND R ADD ( X AS B )`, `SELECT *, x AS b` | `r extend { b := x }` |
| `UPDATE R ( A := X )`, as an expression | `r replace { a := x }` |
| `R WHERE P`, `SELECT * FROM r WHERE p` | `r where p` |
| `TUPLE FROM R` | `tuple from r` |
| `(SELECT b FROM r WHERE p)`, a scalar subquery | `value from (r where p){b}` |
| `R GROUP ( { B, C } AS G )` | `r group { g := {b, c} }` |
| `R UNGROUP ( G )` | `r ungroup {g}` |
| `R WRAP ( { B, C } AS W )`, `R UNWRAP ( W )` | `r wrap { w := {b, c} }`, `r unwrap {w}` |
| `GROUP BY a` with the group's rows | `r group { rows := {…every attribute but a…} }` |
| `PRIMARY KEY (a)`, `UNIQUE (a)`, `KEY { A }` | `key { a }` |
| `NATURAL JOIN`, `INNER JOIN`, `JOIN … USING (a)` | `join` |
| `CROSS JOIN` | `times` |
| `JOIN { R1, R2, R3 }` | `join{r1, r2, r3}` |
| `T1 UNION T2`, of tuples (tuple union) | `t1 join t2` |
| `JOIN … ON r.a = s.b` | `r join s rename {b as a}` |
| `LEFT JOIN`, `RIGHT JOIN`, `FULL JOIN` | none: there are no nulls |
| `a / b` for integers | `a div b` |
| `a % b` | `a mod b` |
| `1 / 2` meaning one half | `1e0 / 2e0` |
| `inf`, `NaN`, `1e0 / 0e0` | an error: approximate numbers are finite |

The compiler recognizes each of these habits and suggests the fix.
`trendyc --explain <code>` describes any error code in detail, and
`trendyc --error-format=json` prints each error as JSON, with its fixes as
exact edits.
