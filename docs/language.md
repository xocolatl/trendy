# The trendy language

A reference for the language as it exists so far. It is written for anyone
writing trendy code, language models included, and every example in it is
compiled by `crates/trendyc/tests/explanations.rs`: blocks marked `trendy`
must compile, and blocks marked `trendy,error` must not.

trendy is a relational language in the spirit of *The Third Manifesto*. Its
syntax is its own, and in particular it is not Tutorial D's.

## Programs

A source file is a sequence of top-level definitions, in any order: operator
definitions, variable declarations and heading declarations (see
[Headings](#headings)). Execution starts in the operator named `main`, which
every program must define.

```trendy
var greeting: text := "hello";

operator main{} [];
```

A program that fails while it runs, as when an integer overflows (R0001),
stops, writing the error to stderr as the compiler writes one, with the
values involved, such as the operands of an operator that has no result,
then where the program was, and exits with status 1. Where it was is the statement
running, or, in a value block, such as a read-only operator's body, the
statement that gives a local variable the value being computed, or the
value the block ends with. Here, a recipe in `projects/shopping` that
serves no one, by its file's line, divides by zero where `shopping_list`
scales each ingredient to the people a meal is cooked for:

```text
error[R0002]: division by zero: 2e1 / 0e0 has no result
 --> projects/shopping/shopping.trendy:90:5
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
  in any order. `main` has none, since nothing passes it arguments. An
  update operator's parameter can be `var` or `out`, written before its
  name: the operator can then assign the variable it is passed (see [`var`
  and `out` parameters](#var-and-out-parameters)).
- **A body is a list of statements,** each ending with `;`: calls,
  assignments (see [Assignment](#assignment)), local variables'
  declarations (see [Local variables](#local-variables)), and in an update
  operator `if` statements (see [`if` statements](#if-statements)) and
  loops (see [Loops](#loops)). A
  read-only operator's ends with its value, an expression without `;`
  (see [Read-only operators](#read-only-operators)). An expression alone
  isn't a statement, as its value would go unused (P0035).
- **Arguments are a tuple of the parameters:** `call greet{name: "Ada",
  count: 2};`. Every argument is named, every parameter gets one, and the
  order doesn't matter. One for a `var` or `out` parameter is marked so
  too.

Following *The Third Manifesto*, operators are either read-only operators,
which return a value and are used in expressions, or update operators, which
return nothing and are invoked by a `call` statement.

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

```trendy,error
operator main{} [
    1 + 2;
];
```

### Names

Inside an operator, a name refers to one of its parameters, or one of its
local variables, or else to a variable of the program: an operator's own
names hide the program's variables with the same name (see [Local
variables](#local-variables)). An operator is not a value: its name follows `call`, or, for a read-only
operator, comes right before its arguments in an expression, `f{x: 1}`.

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

### Operators of one name

Operators of one name are told apart by their parameters' names, which
every call names its arguments with: a call calls the operator whose
parameters its arguments name. So a name can have an operator for each
set of parameters' names, as *The Third Manifesto* permits an operator
name for operators with different parameters (chapter 6, RM Prescription
3), and the prelude's names can have the program's operators too.

```trendy
operator area{width: integer, height: integer}: integer [
    width * height
];

operator area{side: integer}: integer [
    side * side
];

operator count{a: any array}: integer [
    length{a: a}
];

operator main{} [
    call debug_value{v: area{width: 3, height: 4}};
    call debug_value{v: area{side: 3}};
    call debug_value{v: count{a: array [1, 2]}};
    call debug_value{v: count{r: relation { { x: 1 } }}};
];
```

- **Names, not types, tell them apart.** Two operators of one name with
  the same parameters' names, in any order, are one defined twice
  (A0001), whatever their types, so the arguments' names alone say which
  a call calls, before any argument is checked. An operation that works
  on several types, each its own way, is a generic operator of a kind
  instead (see [Kinds](#kinds)).
- **A call that names no operator's parameters** is checked against the
  one whose parameters it names most nearly, each argument it has no
  parameter for and each parameter it doesn't name counting two, and a
  misspelling of one it doesn't name, or an argument without a name,
  one for both: its missing, unknown and unnamed arguments are reported
  (A0009, A0010, A0018), and its error names the other operators. When
  two are equally near, nothing says which was meant, and the call is
  reported (A0056).
- **`main` is the one without parameters.**

```trendy,error
operator greeting{name: text, title: text}: text [ title || " " || name ];
operator greeting{name: text, nickname: text}: text [ nickname ];

operator main{} [
    call debug_value{v: greeting{name: "Ada"}};
];
```

### `var` and `out` parameters

```trendy
var visits := 0;

operator add{var total: integer, const amount: integer} [
    total := total + amount;
];

operator split{n: integer, out tens: integer, out units: integer} [
    tens := n div 10;
    units := n mod 10;
];

operator main{} [
    call add{var total: visits, amount: 42};
    var tens := 0;
    var units := 0;
    call split{n: visits, out tens: tens, out units: units};
    call debug_value{v: tuple { tens: tens, units: units }};
];
```

A parameter takes its argument in one of three modes, written before its
name:

- **`const`, whether written or not,** is how a parameter takes its
  argument unless it says otherwise: the operator only reads the
  argument's value. It can't assign the parameter (A0026); to work on a
  changed value, it declares a local variable for it.
- **`var` takes a variable,** which the operator can read and assign
  through the parameter: the parameter starts with the variable's value,
  and the variable has the parameter's when the operator ends (TTM, RM
  Prescription 3). Only an update operator's parameters can be `var`, as a
  read-only operator changes nothing but its own local variables, and
  neither a `per` parameter nor the relation it is evaluated for each
  tuple of can be (A0051).
- **`out` takes a variable whose value is never read:** the operator
  assigns the parameter before it reads it, and on every path, so the
  variable is a result of the operator, as `tens` and `units` are
  `split`'s. Reading one before it is assigned on every path to the read
  is an error, as is a change that reads it, such as `insert`, or passing
  it to a `var` parameter (A0054): to read the value it is passed, it is
  `var`, which the fix makes it. So `[ n := 42; var x := n; … ]` is right,
  and `[ var x := n; … ]` isn't. An operator that may end without
  assigning one is an error too (A0054): an `if` without `else`, a `while`
  loop and a `for` loop may not run their statements, so what only they
  assign isn't assigned on every path, while a `loop [ … ] while c;` runs
  its block at least once. Otherwise an `out` parameter is a `var` one:
  only an update operator's, and the variable is assigned the
  parameter's value when the operator ends.
- **A call marks each argument as its parameter is,** before its name:
  `var` for a `var` parameter and `out` for an `out` one, so that the call
  shows what it changes, and `const` or nothing for another (A0052).
- **A `var` or `out` argument is a whole variable** (A0026): one of the
  program's, a local variable, or a `var` or `out` parameter of the
  operator calling, passed on. Not an expression, a `const` parameter, an
  array's element, or a variable that a `for` loop goes through or
  computes its array from.
- **A variable has one name in a call** (A0053). It can't be passed to two
  `var` or `out` parameters, nor to one of an operator that uses it by its
  own name too, reading or assigning it, in its body or in an operator it
  calls: what that name gave would depend on whether the parameter is a
  copy of the variable or the variable itself, so neither is the rule.
  Another argument can read it, as its value is computed before the
  call.
- **A variable's keys are checked when the call ends,** as it is assigned
  the parameter's value then (R0008).
- **Other languages' spellings are fixed** (P0038): C#'s `ref`, Swift's
  `inout` and Ada's `in out` before a parameter's name are `var`, and
  Ada's `in` is `const`; a mode before a parameter's type, as Swift writes
  `inout` and Ada `out`, or before an argument's value, as C#'s `ref x`
  and `out x`, Swift's `&x` and Rust's `&mut x` are, is moved before the
  name; and Tutorial D's `UPDATES { p }` is `var` before each parameter it
  lists. An argument left unmarked, as Delphi and Ada write a call, is
  marked (A0052).

```trendy,error
var visits := 0;

operator add{var total: integer, amount: integer} [
    total := total + amount;
];

operator main{} [
    call add{total: visits, amount: 2};
];
```

```trendy,error
var visits := 0;

operator add{var total: integer, amount: integer} [
    total := total + amount;
];

operator main{} [
    call add{var total: visits + 1, amount: 2};
];
```

```trendy,error
var visits := 0;

operator add{var total: integer, amount: integer} [
    total := visits + amount;
];

operator main{} [
    call add{var total: visits, amount: 2};
];
```

```trendy,error
operator add{ref total: integer, amount: integer} [
    total := total + amount;
];

operator main{} [];
```

```trendy,error
operator count_up{out n: integer} [
    var start := n;
    n := start + 1;
];

operator main{} [];
```

```trendy,error
operator sign{x: integer, out s: text} [
    if x < 0 then [
        s := "negative";
    ];
];

operator main{} [];
```

### Read-only operators

A read-only operator has a result type after its parameters, written as a
variable's type is, and its body ends with its value, an expression of
that type, without `;` (TTM RM Prescription 3), which local variables can
name parts of (see [Local variables](#local-variables)). It is Tutorial
D's `WITH … : exp`: names introduced in turn, then the value. A call of
it, `name{arguments}` in an expression, is that value, computed from the
arguments alone: a read-only operator is a function of its arguments.

```trendy
var rate := 3;
var poets := relation {
    { name: "Ada", born: 1815 },
    { name: "Rumi", born: 1207 },
};

operator double{x: integer}: integer [
    x * 2
];

operator scaled{x: integer, rate: integer}: integer [
    double{x: x} * rate
];

operator born_before{
    year: integer,
    poets: relation { name: text, born: integer },
}: relation { name: text, born: integer } [
    poets where born < year
];

var twelve := scaled{x: 2, rate: rate};
var medieval := born_before{year: 1500, poets: poets}{name};
var doubled := poets extend { twice := double{x: born} };

operator main{} [
    call debug_value{v: twelve};
    call debug_value{v: double{x: 21}};
    call debug_table{r: doubled};
    rate := 4;
    call debug_value{v: scaled{x: 2, rate: rate}};
];
```

- **It changes nothing but its own local variables.** It can't assign a
  variable of the program's or `call` an operator, not even `debug_value`
  (A0034), but it can call read-only operators.
- **It reads only its parameters and its own local variables,** not the
  program's variables (A0050). Its calls with the same arguments have the
  same value, whenever they are made (TTM, RM Prescription 3), so a
  variable it needs is passed to it, as `rate` and `poets` are above, as
  a `const` parameter, which the fix adds, with the variable in each
  call. Whatever reads input, the clock or randomness is an update
  operator, which gives its result through an `out` parameter, as
  `load_text_file` does.
- **Its body ends with its value** (A0035), which is its last item, and
  takes no `;` (P0031). An update operator, which has no result type, has
  no value. So `call` invokes only update
  operators, as `call` of a read-only one would lose its value (A0037),
  and only read-only operators are called in expressions, as an update
  operator has no value (A0016).
- **`return` is for leaving an operator early,** which no operator does
  yet: a body runs to its end (A0035). So a body's value is written alone,
  and `return x;` at its end is fixed to `x` (P0031).
- **It doesn't call itself,** directly or through others (A0036): without
  a conditional, nothing could stop the calls.
- **A call is evaluated when compiling where its arguments are known
  then:** in an initial value, where variables have their initial values,
  and in a body, as `double{x: 21}` is, and in an `extend`'s or a
  `where`'s values, for each tuple of a constant operand. A call that
  fails, such as one that overflows for its arguments, is an error at the
  call.
- **Elsewhere it is made when the program runs:** where an argument is
  known only then, as a variable's value is in a body, so
  `scaled{x: 2, rate: rate}` above is 16 once `rate` is 4, and a failure
  in it stops the program, as one anywhere else does.
- **`f{}` calls a read-only operator without parameters,** and is its
  value, even where a variable or parameter is named `f` too: see
  [Names](#names-1).

```trendy,error
var calls := 0;

operator double{x: integer}: integer [
    calls := calls + 1;
    x * 2
];

operator main{} [];
```

```trendy,error
var rate := 3;

operator scaled{x: integer}: integer [
    x * rate
];

operator main{} [];
```

```trendy,error
operator double{x: integer}: integer [
    x * 2
];

operator main{} [
    call double{x: 21};
];
```

### Generic operators

A parameter's type can be `any type`, `any tuple`, `any relation` or
`any array`: the operator takes an argument of any type, of any tuple
type, of any relation type, or of any array type, and each call gives the
parameter its argument's type. It
can also be `any` and a kind, a set of types (see [Kinds](#kinds)). A
type parameter, declared after the signature with `types`, names such a
type, to say that several types are one, as for `same`'s `a` and `b`, or
that the result's type is made of an argument's, as for `pair`.

```trendy
var cities := relation {
    { city: "Kinshasa", river: "Congo" },
    { city: "Brazzaville", river: "Congo" },
    { city: "Varanasi", river: "Ganges" },
}
key { city };

operator show{v: any type} [
    call debug_value{v: v};
];

operator same{a: T, b: T}: boolean
    types { T: any type }
[
    a = b
];

operator pair{a: T, b: T}: relation { v: T }
    types { T: any type }
[
    relation {
        tuple { v: a },
        tuple { v: b }
    }
];

operator within{r: R, of: R}: boolean
    types { R: any relation }
[
    r <= of
];

operator main{} [
    call show{v: cities};
    call show{v: same{a: "Congo", b: "Ganges"}};
    call show{v: pair{a: 1, b: 2}};
    call show{v: pair{a: cities{river}, b: cities{river}}};
    call show{v: within{r: cities where river = "Congo", of: cities}};
];
```

*The Third Manifesto* asks for operators like these (RM Very Strong
Suggestion 7): the built-in relational operators are generic, working for
every relation type, and a program should be able to define its own,
though the Manifesto leaves how to further study.

- **`types { … }` declares the type parameters,** after the parameters
  and the result type: each a name, capitalized by convention, and what
  it can be, `any type`, `any tuple`, `any relation` or `any array`
  (A0040). They are in braces, so their order means nothing, as the
  parameters' doesn't: `types { T: any type, U: any type }` and
  `types { U: any type, T: any type }` declare the same. Inside the
  operator, a type parameter is a type, which the parameters' types, the
  result type and the body can use, in other types too, as in
  `relation { v: T }`. Each `any …` written as a parameter's type is a type
  parameter without a name, of its own: `a: any type, b: any type` can
  take arguments of two types.
- **Each call gives each type parameter a type,** from its arguments: the
  type of the argument whose parameter's type it is, or the part of that
  argument's type where it is, so a call never names types. Every
  argument it gives a type must give it the same one (A0004). So each
  type parameter must be in some parameter's type (A0038), and one written
  only once, as a parameter's type, is written `any type` instead, without
  a name (A0039). An argument is checked against its parameter's type
  once the type parameters in it have types, which other arguments can
  give them: for parameters `a: T` and `r: relation { v: T }`, with `1` for
  `a`, `relation {}` for `r` is the empty `relation { v: integer }`. Which
  argument gives a type parameter its type never depends on the order the
  arguments are written in.
- **A body is checked once, for every type.** Only what applies to every
  type a type parameter can be applies to its values: `=` and `<>`, and
  passing them on, to operators and into tuples and relations; for
  `any relation`, which is ordered by inclusion, `<=`, `<`, `>=` and `>`;
  for `any array`, `length`; and for a kind, what it requires. So `a < b`
  of `T`s of `any type` is an error (A0022), even if every call gives `T`
  integers, whose help says to make it `any ordered`; and so are the
  attributes of `any tuple` and `any relation`, and the elements of
  `any array`, which aren't known: to index an array, give its elements a
  type parameter of their own, as in `a: array E`.
- **Each list of types is compiled on its own,** as if the operator were
  written again with the types in place of its type parameters, and a call
  is evaluated when compiling where it can be, as any other is. So a call
  that leads back to the operator making it, giving a type parameter a
  type built from that operator's own, such as `tuple { x: T }`, is an
  error (A0041): it would need a version for every larger type.
- **Only parameters have them.** A variable's type is always a specific
  type, and `any type` written anywhere but as a parameter's type is an
  error (A0014). `any`, `type` and `types` are words of their own only
  there, so a variable can be named `any` or `types`.
- **Other languages' spellings are fixed:** type parameters in angle
  brackets after the name, `same<T>{…}`, as Rust, TypeScript, Java and C++
  write them, to `types { T: any type }`, `relation any` to `any relation`
  and `array any` to `any array` (P0027), and a bare `any`, as TypeScript and Go write it, to `any type`
  (A0003).

```trendy,error
operator same{a: T, b: T}: boolean
    types { T: any type }
[
    a = b
];

var different := same{a: 1, b: "Lima"};

operator main{} [];
```

```trendy,error
operator less{a: T, b: T}: boolean
    types { T: any type }
[
    a < b
];

operator main{} [];
```

```trendy,error
operator same<T>{a: T, b: T}: boolean [
    a = b
];

operator main{} [];
```

### Kinds

A kind is a set of types, each of which declares it, and what each of them
must have. Only the prelude declares kinds, so far two: `ordered`, the
types with a total order, `<`, and `numeric`, the types with addition and
multiplication, `+` and `*`. A type parameter of `any ordered` can be
compared with `<`, `<=`, `>` and `>=`, one of `any numeric` added and
multiplied, and a call gives it one of the kind's types.

```trendy
var plots := relation {
    { plot: 1, region: "Nyeri", rainfall: 1200 },
    { plot: 2, region: "Nyeri", rainfall: 950 },
    { plot: 3, region: "Mendoza", rainfall: 220 },
}
key { plot };

operator below{r: any relation, of: T per r, limit: T}: relation { v: T }
    types { T: any ordered }
[
    (r where of < limit){of} rename {of as v}
];

operator main{} [
    call debug_table{r: below{r: plots, of: rainfall, limit: 1000}};
    call debug_table{r: below{r: plots, of: region, limit: "N"}};
];
```

- **`any` is followed by a set of types:** `type`, every type; `tuple` or
  `relation`, every type that type generator makes, whatever its heading;
  or a kind, every type that declares it. The first three come from how
  types are made, so the language has them; kinds are declared, so that
  nothing about them is built into the compiler.
- **A type declares the kinds it is of,** as the prelude's do:
  `builtin type integer: { ordered, numeric };`. It must have what each
  requires, which the prelude's loading checks. `boolean`, `integer`,
  `approximate` and `text` are ordered; relations aren't, as their `<` is
  subset, which isn't a total order. `integer` and `approximate` are
  numeric. A type of the program's own is ordered when its braces give
  it an order (see [A program's own types](#a-programs-own-types)), and
  can't be numeric yet.
- **A kind says what its types must have, not what they happen to have,**
  so a body can use exactly what its type parameters' kinds require, and
  nothing else. Declaring a kind also says what no compiler can check, as
  that `<` is a total order.
- **A type parameter is of one kind,** and no kind yet requires both `<`
  and `+`, so a body can't both compare and add its values: the help on
  using one says what switching kinds would lose (A0022).
- **No literal is of a type parameter,** since each call gives it a type:
  `x + 1` of an `x` of `any numeric` is an error (A0004), whose note says
  that values of the type come from the arguments.
- **An argument must be of the kind** (A0004), and a kind named wrong is
  reported with the closest as the fix (A0003). A program can't declare
  a kind (A0013): a kind's requirements are operators only the compiler
  gives, so only the prelude declares one.

```trendy
operator cost{price: T, quantity: T, delivery: T}: T
    types { T: any numeric }
[
    price * quantity + delivery
];

operator main{} [
    call debug_value{v: cost{price: 1250, quantity: 3, delivery: 400}};
    call debug_value{v: cost{price: 2.5e0, quantity: 1.5e0, delivery: 4e0}};
];
```

```trendy,error
operator before{a: T, b: T}: boolean types { T: any ordered } [
    a < b
];

var x := before{a: tuple { n: 1 }, b: tuple { n: 2 }};

operator main{} [];
```

### `per` parameters

A parameter written `name: type per r` takes an argument evaluated for
each tuple of the argument for `r`, another parameter, of type
`any relation`, with that tuple's attributes in scope by name, as in
`where`. Inside the operator, it is an attribute of `r`'s tuples.

```trendy
var plots := relation {
    { plot: 1, region: "Nyeri", rainfall: 1200 },
    { plot: 2, region: "Nyeri", rainfall: 950 },
    { plot: 3, region: "Mendoza", rainfall: 220 },
}
key { plot };

operator values{r: any relation, of: T per r}: relation { v: T }
    types { T: any type }
[
    r{of} rename {of as v}
];

operator holds_for_any{r: any relation, holds: boolean per r}: boolean [
    not is_empty{r: r where holds}
];

operator main{} [
    call debug_table{r: values{r: plots, of: region}};
    call debug_table{r: values{r: plots where region = "Nyeri", of: rainfall div 100}};
    call debug_value{v: holds_for_any{r: plots, holds: rainfall < 500}};
];
```

This is what aggregate operators need: Tutorial D's take a relation and
one of its attributes, as in `SUM (SP, QTY)`, and a `per` argument
generalizes that attribute to any expression of each tuple's attributes.

- **The argument names the tuple's attributes bare,** as `where` and
  `extend` do: `of: region`. Other names refer to what they do around the
  call.
- **The argument must use the tuple:** its attributes, or an image
  relation of it, `!!s`. One that doesn't, such as a constant, a
  parameter, or an attribute of a tuple further out, is the same for
  every tuple, so the result would depend only on how many there are;
  that is an error (A0044), as Tutorial D's attribute is always one of
  the relation's. `of: "region"`, in quotes, as SQL names a column, is a
  text, and the fix makes it the attribute. For a value the same for
  every tuple, use it outside the operator, with `count{r: r}` for how
  many tuples there are.
- **Inside the operator, a `per` parameter is an attribute** of its
  relation's tuples, not a value: it is named where they are in scope, as
  in `r{of}`, `r where of > 0`, `r extend { … }`, or the argument for
  another operator's `per` parameter, `values{r: r, of: of}`. Anywhere else
  it is an error (A0043).
- **The operator gets all of the relation,** never a projection: each
  tuple keeps its own attributes, beside the `per` ones, wrapped in one
  more that the operator can't name. So tuples with equal values of `of`
  stay distinct, and nothing the operator names can clash with their
  attributes: a relation with an attribute called `of` can be given for
  `r`. A type writes the wrapped attribute `…`: inside `values`, `r` is a
  `relation { of: T, … }`. So does `debug_value`, which writes what the
  operator sees: `{ of: "Nyeri", … }` for each tuple.
- **`per` names a parameter of type `any relation`** (A0042), not the one
  it follows, nor another `per` parameter, and only an operator's
  parameters have it. A relation of a known heading needs none: the
  operator can name its attributes itself.
- **Each tuple's value is computed with the call,** when compiling where
  it can be; a failure for one is an error at the argument, with a note
  on the tuple.

```trendy,error
operator values{r: any relation, of: T per r}: relation { v: T }
    types { T: any type }
[
    r{of} rename {of as v}
];

operator first{r: any relation, of: integer per r}: integer [
    of
];

operator main{} [];
```

```trendy,error
var plots := relation {
    { plot: 1, region: "Nyeri", rainfall: 1200 },
    { plot: 2, region: "Mendoza", rainfall: 220 },
}
key { plot };

var wettest := max{r: plots, of: "rainfall"};

operator main{} [];
```

### The prelude

Some operators are part of every program, always in scope: the
*prelude*'s, trendy source the compiler includes. A few the compiler
implements itself, the *built-ins*, which the prelude declares with
`builtin`, as it does the built-in types; the rest it defines in trendy, as
any program could. They are checked once, as a program's own operators
are, and compiled into a program only if it calls them. A program can't
declare built-ins, nor define an operator with a name the prelude has.

The prelude keeps built-ins as few as possible: what can be written in
trendy is, so that it means exactly what its definition says, and when the
language can express more, built-ins become ordinary operators.

```
/* The types with a total order, `<`: a type parameter of `any ordered`
   can be compared with `<`, `<=`, `>` and `>=`. A type of the program's
   own is of it when its braces give it an order, `order [asc …]`. */
kind ordered { < };

/* The types with addition and multiplication, `+` and `*`: values of a
   type parameter of `any numeric` can be added and multiplied. */
kind numeric { +, * };

builtin type boolean: { ordered };
builtin type integer: { ordered, numeric };
builtin type approximate: { ordered, numeric };
builtin type text: { ordered };

builtin operator debug_value{v: any type};

/* Write `r` to stderr as a table: a column for each attribute, in name
   order, and a line for each tuple. */
builtin operator debug_table{r: any relation};

builtin operator count{r: any relation}: integer;

/* Whether `r` has no tuples, Tutorial D's `IS_EMPTY`. */
operator is_empty{r: any relation}: boolean [
    count{r: r} = 0
];

/* The number of elements of `a`. */
builtin operator length{a: any array}: integer;

/* The least value of `of` for a tuple of `r`, by `<`. Of no tuples, there
   is none, and the program fails (R0010). */
builtin operator min{r: any relation, of: T per r}: T
    types { T: any ordered };

/* The greatest value of `of` for a tuple of `r`, by `<`. Of no tuples,
   there is none, and the program fails (R0010). */
builtin operator max{r: any relation, of: T per r}: T
    types { T: any ordered };

/* The sum of `of` for the tuples of `r`, each tuple's counted, though
   two have the same value: the exact sum, rounded once, so that no order
   of adding them matters. Of no tuples, it is 0, or 0e0. A sum beyond
   its type's range fails (R0001). */
builtin operator sum{r: any relation, of: T per r}: T
    types { T: any numeric };

/* The product of `of` for the tuples of `r`, each tuple's counted, though
   two have the same value: the exact product, rounded once. Of no tuples,
   it is 1, or 1e0, and with a zero, zero. A product beyond its type's
   range fails (R0001). */
builtin operator product{r: any relation, of: T per r}: T
    types { T: any numeric };

/* Whether `of` is true for some tuple of `r`: whether `r where of` has
   one. Of no tuples, it is false. */
operator some{r: any relation, of: boolean per r}: boolean [
    not is_empty{r: r where of}
];

/* Whether `of` is true for every tuple of `r`: whether no tuple makes it
   false. Of no tuples, it is true. */
operator every{r: any relation, of: boolean per r}: boolean [
    is_empty{r: r where not of}
];

/* `from` as trendy source, the text `debug_value` writes: `42`, `5e-1`,
   `"Lima"` with its quotes, or `tuple { city: "Lima" }`. */
builtin operator to_text{from: any type}: text;

/* The approximate number nearest `from`: `from` itself up to 2^53 in
   magnitude, beyond which not every integer is one. */
builtin operator to_approximate{from: integer}: approximate;

/* The integer `from` writes, as trendy writes one: decimal digits, with
   a `-` before them if it is negative, and nothing else, not even
   spaces. Other text fails (R0014), as does an integer out of range. */
builtin operator parse_integer{from: text}: integer;

/* The approximate number nearest the decimal number `from` writes:
   digits, then a `.` and more digits if it has a fraction, then an
   exponent if it has one, `e` or `E`, an optional sign and digits, all
   with a `-` before them if it is negative, and nothing else. So `2.5`
   is `2.5e0`, as data files write it, though a literal needs its
   exponent. Other text fails (R0014), as does a number out of range. */
builtin operator parse_approximate{from: text}: approximate;

/* The boolean `from` writes: true for `true`, `t`, `yes`, `y` or `1`,
   and false for `false`, `f`, `no`, `n` or `0`, in any case, and nothing
   else, not even spaces. Other text fails (R0014). */
builtin operator parse_boolean{from: text}: boolean;

/* π, the ratio of a circle's circumference to its diameter: the
   approximate number nearest it. */
operator pi{}: approximate [
    3.141592653589793e0
];

/* The square root of `of`: the approximate number nearest it, correctly
   rounded, as IEEE 754 has it. A negative number has none, and the program
   fails (R0018). */
builtin operator sqrt{of: approximate}: approximate;

/* The sine of `of`, an angle in radians. */
builtin operator sin{of: approximate}: approximate;

/* The cosine of `of`, an angle in radians. */
builtin operator cos{of: approximate}: approximate;

/* The angle, in radians, from the positive x axis to the point (x, y), from
   -π, exclusive, to π: 0 for the origin, as for any point on the positive x
   axis. */
builtin operator atan2{y: approximate, x: approximate}: approximate;

/* Read the lines of the text file at `path` into `lines`, in the file's
   order, so that its first line is the array's element 1. `path` is
   relative to the directory the program runs in, unless it is absolute.
   A line ends at a line feed, or a carriage return and a line feed,
   which aren't part of it; the last needs neither. Each call reads the
   file as it is when the program runs: the compiler never opens it, so
   it needn't exist until then, and two calls can read different lines,
   so it is an update operator, which `call` invokes. A file that can't
   be read fails (R0012), as does one that isn't UTF-8 text (R0013). */
builtin operator load_text_file{path: text, out lines: array text};

/* Read the records of the CSV file at `path` into `rows`, in the file's
   order, each a tuple whose attributes are the fields in the columns
   `columns` gives them: it has the tuples' attributes, each the position
   of its column, counting from 1, as `tuple { city: 1, people: 3 }`
   reads `city` from the first column and `people` from the third. A
   file's columns have an order and no names, so the call names them.
   Columns it doesn't name are skipped. With `header`, the file's first
   record is a header, which is skipped too. Each attribute is text, an
   integer, an approximate number or a boolean, read from its field as
   `parse_integer`, `parse_approximate` and `parse_boolean` read them,
   and text as it is, or a value of a type of the program's own whose
   components are, in one of its possible representations, which
   `columns` gives a tuple of their columns, as
   `tuple { name: 1, at: tuple { latitude: 2, longitude: 3 } }` does, and
   which is selected from them, so that one whose components break its
   type's constraints fails (R0017). The file is CSV as RFC 4180 has it:
   fields separated by commas, records by line ends, as for
   `load_text_file`, and a field with a comma, a quote or a line end in
   quotes, each quote in it doubled. Empty lines are skipped. A file that can't be read
   fails (R0012), as does one that isn't UTF-8 text (R0013), or isn't
   CSV (R0015), or hasn't a column `columns` gives, which two attributes
   can't share (R0016), or that has a field its attribute's type can't
   read (R0014). */
builtin operator load_csv{path: text, header: boolean, columns: C, out rows: array T}
    types { C: any tuple, T: any tuple };
```

**`debug_value{v: any type}`** writes `v` to stderr, on one line, as trendy
source. Tuples and relations are written with their attributes in name
order, relations with their heading first and their tuples in a fixed order,
and an empty array with its elements' type, as in `array integer []`, so
the output reads back as the same value. For example:
`relation { a: boolean, b: integer } { { a: false, b: 25 }, { a: true, b: 3 } }`.
The one exception is the attribute a `per` parameter's relation wraps its
tuples' own attributes in, which no source can name: it is written `…`,
as in `relation { of: text, … } { { of: "Nyeri", … } }`.

**`debug_table{r: any relation}`** writes `r` to stderr as a table, for
reading rather than as source: a column for each attribute, in name order,
headed by its name, and a line for each tuple, in the same order as
`debug_value`'s. A cell is as wide as its column's widest, in a
terminal's columns, which a wide character, such as `李`, takes two of.
Numbers go to the right, and everything else to the left: a text without
its quotes, its control characters escaped as in a literal, and a tuple,
a relation or an array as `debug_value` writes it.

```trendy
var cities := relation {
    { city: "Lima", native: "Lima", millions: 10 },
    { city: "Xi'an", native: "西安", millions: 13 },
    { city: "Kyiv", native: "Київ", millions: 3 },
};

operator main{} [
    call debug_table{r: cities};
];
```

writes

```
┌───────┬──────────┬────────┐
│ city  │ millions │ native │
├───────┼──────────┼────────┤
│ Kyiv  │        3 │ Київ   │
│ Lima  │       10 │ Lima   │
│ Xi'an │       13 │ 西安   │
└───────┴──────────┴────────┘
```

**`count{r: any relation}: integer`** is the number of tuples of `r`,
Tutorial D's `COUNT` and SQL's `COUNT(*)`. With an image relation, it
counts each tuple's matches, keeping the groups with none, which SQL's
`GROUP BY` leaves out: below, Hokkaido has no plots.

```trendy
var plots := relation {
    { plot: 1, region: "Nyeri" },
    { plot: 2, region: "Nyeri" },
    { plot: 3, region: "Mendoza" },
}
key { plot };

var regions := relation { { region: "Nyeri" }, { region: "Mendoza" }, { region: "Hokkaido" } };
var counted := regions extend { plots := count{r: !!plots} };

operator main{} [
    call debug_table{r: counted};
];
```

**`is_empty{r: any relation}: boolean`** is whether `r` has no tuples,
Tutorial D's `IS_EMPTY`, written in the prelude as `count{r: r} = 0`.
With an image relation, it asks whether a group has no matches, as SQL's
`NOT EXISTS` does: below, which regions have no plots.

```trendy
var plots := relation {
    { plot: 1, region: "Nyeri" },
    { plot: 2, region: "Nyeri" },
    { plot: 3, region: "Mendoza" },
}
key { plot };

var regions := relation { { region: "Nyeri" }, { region: "Mendoza" }, { region: "Hokkaido" } };
var unplanted := regions where is_empty{r: !!plots};

operator main{} [
    call debug_table{r: unplanted};
];
```

**`min{r: any relation, of: T per r}: T`** and
**`max{r: any relation, of: T per r}: T`**, for `T` of `any ordered`, are
the least and the greatest value of `of` for a tuple of `r`, by `<`:
Tutorial D's `MIN` and `MAX`, and SQL's. `of` can be of any ordered type,
so text too, by code point. Of no tuples, neither has a value, so either
is an error, when compiling if the relation is known then, and otherwise
when the program runs (R0010); SQL's would be `NULL`, which trendy doesn't
have. A group's image relation has no tuples when nothing matches the
group, so for each group's, extend only those with tuples, which
`matching` keeps: below, Hokkaido has no plots.

```trendy
var plots := relation {
    { plot: 1, region: "Nyeri", rainfall: 1200 },
    { plot: 2, region: "Nyeri", rainfall: 950 },
    { plot: 3, region: "Mendoza", rainfall: 220 },
}
key { plot };

var regions := relation { { region: "Nyeri" }, { region: "Mendoza" }, { region: "Hokkaido" } };
var driest := min{r: plots, of: rainfall};
var wettest := (regions matching plots) extend { rainfall := max{r: !!plots, of: rainfall} };

operator main{} [
    call debug_value{v: driest};
    call debug_table{r: wettest};
];
```

```trendy,error
var plots := relation { plot: integer, rainfall: integer } {};
var driest := min{r: plots, of: rainfall};

operator main{} [];
```

**`sum{r: any relation, of: T per r}: T`**, for `T` of `any numeric`, is
the sum of `of` for the tuples of `r`: Tutorial D's `SUM`, and SQL's.

- **Each tuple's value counts,** even where two tuples' are equal: below,
  both of Punjab's 950 tonnes. Projecting `r` on the attribute first would
  merge them, so a call with a projection and no `of`, as in
  `sum{r: harvests{tonnes}}`, is fixed to name the attribute as `of`
  (A0009).
- **Of no tuples, it is 0,** or `0e0` for approximate numbers: the sum of
  no numbers is zero, as TTM has it (RM Prescription 10). A group's image
  relation with no tuples sums to 0 too, so every group stays, unlike
  with SQL's `GROUP BY`, which would leave Puno out, or give `NULL`.
- **It is exact, then rounded once,** since a relation's tuples have no
  order to add them in: the sum is the one nearest the exact sum of the
  values, whatever order they are stored in. Below, `1e-1`, `2e-1` and
  `3e-1` sum to `6e-1`, where `1e-1 + 2e-1 + 3e-1` is
  `6.000000000000001e-1`, rounded at each `+`; and an integer sum fails
  only if the exact sum is out of range (R0001), even where adding in some
  order would overflow on the way.
- **There is no mean yet,** since the mean of integers isn't an integer:
  divide the sum by the count, `sum{r: r, of: x} / to_approximate{from:
  count{r: r}}`, converting an integer sum with `to_approximate` too.
  `avg{…}` is an error that says so (A0015).

```trendy
var harvests := relation {
    { region: "Nyeri", crop: "tea", tonnes: 120 },
    { region: "Nyeri", crop: "coffee", tonnes: 45 },
    { region: "Punjab", crop: "wheat", tonnes: 950 },
    { region: "Punjab", crop: "rice", tonnes: 950 },
}
key { region, crop };

var regions := relation { { region: "Nyeri" }, { region: "Punjab" }, { region: "Puno" } };
var total := sum{r: harvests, of: tonnes};
var by_region := regions extend { tonnes := sum{r: !!harvests, of: tonnes} };
var tenths := relation { { n: 1, x: 1e-1 }, { n: 2, x: 2e-1 }, { n: 3, x: 3e-1 } };

operator main{} [
    call debug_value{v: total};
    call debug_table{r: by_region};
    call debug_value{v: sum{r: tenths, of: x}};
];
```

```trendy,error
var harvests := relation {
    { region: "Punjab", crop: "wheat", tonnes: 950 },
    { region: "Punjab", crop: "rice", tonnes: 950 },
};
var total := sum{r: harvests{tonnes}};

operator main{} [];
```

**`product{r: any relation, of: T per r}: T`**, for `T` of `any numeric`,
is the product of `of` for the tuples of `r`. Tutorial D has no product
aggregate, and SQL none either, which writes `EXP(SUM(LN(x)))` of positive
numbers; TTM gives its value of no tuples: "the product of no numbers is
one" (RM Prescription 10).

- **Of no tuples, it is 1,** or `1e0`, and with a zero factor, zero,
  whatever the others, of the sign of the rest for approximate numbers.
- **It is exact, then rounded once,** as `sum` is. Below, the rates
  compound to `1.0804500000000001e0`, the nearest to their exact product,
  where `1.05e0 * 1.05e0 * 9.8e-1` is `1.08045e0`, rounded at each `*`. A
  product fails only if the exact product is out of range (R0001), so a
  partial product too large or too small on the way doesn't matter, and
  of integers, `-9223372036854775808` is one, though its magnitude isn't.
- **Other languages' names are fixed:** Python's `prod`, and `len` for
  `count` (A0015).

```trendy
var rates := relation {
    { year: 2021, rate: 1.05e0 },
    { year: 2022, rate: 1.05e0 },
    { year: 2023, rate: 9.8e-1 },
};
var compounded := product{r: rates, of: rate};
var in_turn := 1.05e0 * 1.05e0 * 9.8e-1;

operator main{} [
    call debug_value{v: compounded};
    call debug_value{v: in_turn};
];
```

**`some{r: any relation, of: boolean per r}`** and
**`every{r: any relation, of: boolean per r}`** are aggregates, SQL's
`SOME` and `EVERY`, and Tutorial D's `OR` and `AND` of a relation: whether
`of` is true for some tuple of `r`, or for every one. Of no tuples, `some`
is false and `every` true.

```trendy
var plots := relation {
    { plot: 1, region: "Nyeri", rainfall: 1200 },
    { plot: 2, region: "Mendoza", rainfall: 220 },
}
key { plot };

var any_dry := some{r: plots, of: rainfall < 500};
var all_wet := every{r: plots, of: rainfall > 500};

operator main{} [
    call debug_value{v: any_dry};
    call debug_value{v: all_wet};
];
```

**`to_text{from: any type}: text`**,
**`to_approximate{from: integer}: approximate`**,
**`parse_integer{from: text}: integer`**,
**`parse_approximate{from: text}: approximate`** and
**`parse_boolean{from: text}: boolean`** convert values, which
never convert implicitly: *The Third Manifesto* allows no coercion, only
explicit conversions such as Tutorial D's `CAST_AS_CHAR` (chapter 3).

- **`to_text` writes any value as trendy source,** the text `debug_value`
  writes, so a value of every type has one, and it reads back as the same
  value: `42`, `5e-1` for an approximate number, which is always written
  with an exponent, and `"Lima"` for a text, with its quotes, which a
  text never needs, being text already.
- **`to_approximate` is the approximate number nearest an integer,** the
  integer itself up to 2^53 in magnitude, beyond which not every integer
  is one, so `9007199254740993` becomes `9.007199254740992e15`. There is
  no conversion back yet: an approximate number has to be rounded to be
  an integer, and how is the program's choice.
- **`parse_integer` reads an integer as trendy writes one:** decimal
  digits, with a `-` before them if it is negative, and nothing else, so
  not `+5`, `1_000` or `4.5`, and not ` 42`, with a space.
- **`parse_approximate` reads a decimal number,** with or without a
  fraction or an exponent: `2.5`, `6650` and `1.5e-3`, but not `.5`, `5.`,
  `inf` or `NaN`. So it reads every number `to_text` writes, and the
  decimals data files write, though a literal needs its exponent: in the
  source, only the literal's form says its type, which is known here. A
  number too small to tell from zero is zero, and there is one zero.
- **`parse_boolean` reads the words data writes booleans with:** `true`,
  `t`, `yes`, `y` and `1` for true, and `false`, `f`, `no`, `n` and `0`
  for false, in any case, so `TRUE`, `Yes` and `N` too, but not ` yes`,
  with a space, nor `on`. It reads what `to_text` writes, `true` and
  `false`. Text that writes a boolean otherwise is compared with the
  text for true instead: `answer = "on"`.
- **Text that isn't a value of the type fails** (R0014), as does a number
  out of its range: when compiling, if the text is known then, and
  otherwise when the program runs, which writes the text, as in
  `error[R0014]: "4.5" isn't an integer`.
- **The errors fix it:** a value of the wrong type where text or an
  approximate number is expected, as in `"n = " || n` or `x * n`, has a fix
  that converts it, and so does text where a number or a boolean is
  expected, which is parsed, unless it is known when compiling: text
  that parses, as in `"42"` or `"yes"`, is rewritten as the literal of
  its value instead. `parseInt`, `parseFloat`, `atoi`, `atof`,
  `parseBoolean` and `ParseBool` are fixed to `parse_integer`,
  `parse_approximate` and `parse_boolean`.

```trendy
var cities := relation {
    { city: "Lima", people: 10883000, km2: 2672.3e0 },
    { city: "Hanoi", people: 8435700, km2: 3358.6e0 },
};
var densities := cities extend { density := to_approximate{from: people} / km2 };

operator describe{city: text, people: integer} [
    call debug_value{v: city || " has " || to_text{from: people} || " people"};
];

/* A city's people, area and whether it is a capital, from text, read
   when the program runs. */
operator read{people: text, km2: text, capital: text} [
    call debug_value{v: parse_integer{from: people}};
    call debug_value{v: parse_approximate{from: km2}};
    call debug_value{v: parse_boolean{from: capital}};
];

operator main{} [
    call debug_table{r: densities{city, density}};
    call describe{city: "Lima", people: 10883000};
    call read{people: "8435700", km2: "3358.6", capital: "Y"};
];
```

```trendy,error
operator describe{city: text, people: integer} [
    call debug_value{v: city || " has " || people || " people"};
];

operator main{} [];
```

```trendy,error
var people := parse_integer{from: "8.4e6"};

operator main{} [];
```

**`load_text_file{path: text, out lines: array text}`** reads input: the
lines of a text file, into the variable its call gives `lines`.

```trendy
operator main{} [
    var lines: array text;
    call load_text_file{path: "rivers.txt", out lines: lines};
    call debug_value{v: length{a: lines}};
    for line in lines loop [
        if line <> "" then [
            call debug_value{v: line};
        ];
    ];
];
```

- **It is an update operator,** which `call` invokes, not a read-only
  one: reading a file twice can give different lines, and a read-only
  operator is a function, whose calls with the same arguments give the
  same value (TTM, RM Prescription 3). So it is called in an update
  operator's body, when the program runs, and never in an expression or
  a variable's initial value, which is computed when compiling (A0016).
  Its result is its `out` parameter's (see [`var` and `out`
  parameters](#var-and-out-parameters)); written as a value, as other
  languages return one, it is fixed to a call with the variable its `out`
  argument.
- **The lines are an array, in the file's order,** each without the line
  feed, or the carriage return and line feed, that ends it. So the first
  line is element 1, and two equal lines are two elements: a file's lines
  have an order and can repeat, which a relation of them would have to
  spell out with each one's position, as `load r from lines as line
  numbered n` does (see [Loading an array](#loading-an-array)).
- **The last line needs no ending,** so a file that ends with one has no
  empty line after it, and an empty file has no lines.
- **The path is relative to the directory the program runs in,** not the
  one its source is in, unless it is absolute.
- **Each call reads the file as it is when the program runs.** The
  compiler never opens it, so the file needn't exist until the program
  runs, and it can change between two runs, or between two calls.
- **A file that can't be read stops the program** (R0012), writing its
  path and the system's reason, as does one that isn't UTF-8 text
  (R0013), writing the first line that isn't.

```trendy,error
operator main{} [
    var lines := load_text_file{path: "rivers.txt"};
];
```

```trendy,error
var lines := load_text_file{path: "rivers.txt"};

operator main{} [];
```

**`load_csv{path: text, header: boolean, columns: C, out rows: array T}`**,
where `C` and `T` are any tuples, reads the records of a CSV file into the
variable its call gives `rows`, each a tuple of the columns `columns` gives
its attributes.

```trendy
heading city := { name: text, people: integer, km2: approximate, capital: boolean };

operator main{} [
    var rows: array tuple city;
    call load_csv{
        path: "cities.csv",
        header: true,
        columns: tuple { name: 1, capital: 2, km2: 4, people: 5 },
        out rows: rows,
    };
    var cities: relation city;
    load cities from rows;
    call debug_table{r: cities where capital};
];
```

- **The call names the columns.** A file's columns have an order and no
  names, but a tuple's attributes have names and no order (*The Third
  Manifesto*, RM Proscription 1), so `columns` gives each attribute of
  the tuples the position of its column, counting from 1, as an array's
  positions do: a tuple of the tuples' attributes, each an integer, with
  no other attribute (A0009, A0010, A0004). It is the call, not the
  file, that says which column is which, as SQL's `INSERT INTO t (a, b)`
  does, so a header's names are data, and the program's names its own.
  Columns `columns` doesn't name are skipped. Two attributes can't have
  one column, and a position below 1, or past the file's last column,
  is no column (R0016), which is an error when compiling if the
  positions are known then, and otherwise stops the program.
- **`header` says whether the first record is a header,** which is then
  skipped, as RFC 4180's `header=present` says. With `header: false`,
  the first record is a row, and if one of its fields isn't its
  attribute's type, the error says the line may be a header.
- **The array's type says what to read.** Each field is read as its
  attribute's type, as Tutorial D's `LOAD` takes its shape from its
  target (chapter 5): text as it is, and an integer, an approximate
  number or a boolean as `parse_integer`, `parse_approximate` and
  `parse_boolean` read them, so `2672.3` is an approximate number, and
  `yes` and `N` are booleans. A field that isn't one stops the program
  (R0014), writing it, its column, with the header's name for it if
  there is one, its line, and its attribute. An attribute of another
  type, which no field writes, is an error when compiling (A0055), as is
  an array whose tuples' type is a type parameter, whose attributes
  aren't known then.
- **A value of a type of the program's own is read from its components'
  columns,** which `columns` gives it as a tuple of them, nested for a
  value inside: a position of a city is
  `at: tuple { latitude: 6, longitude: 7 }`. They can be the components
  of any of its possible representations, those of one (A0059), so that
  a file's temperatures in degrees Celsius are
  `mean: tuple { celsius: 3 }`, converted by its `to` to the stored
  representation. The value is then selected from them, so one whose
  components break its type's constraints stops the program (R0017),
  writing the record's line, the attribute, the value and the
  constraint, a value inside one first. Errors name a component as `t.c`
  reads it, as in `at.latitude`.
- **The file is CSV as RFC 4180 has it:** fields separated by commas,
  and records by line ends, as for `load_text_file`; a field with a
  comma, a quote or a line end is in quotes, each quote in it doubled,
  as in `"Hanoi, ""Hà Nội"""`. A field's spaces are part of it, so
  ` 42` isn't an integer. Empty lines are skipped, and so is a byte order
  mark at the start. A file that isn't CSV, or has a record with another
  number of fields than the first, stops the program (R0015), writing
  the line.
- **The rows are an array, in the file's order,** which can repeat, as
  `load_text_file`'s lines can; `load r from rows` makes a relation of
  them, in which equal ones are one (see [Loading an
  array](#loading-an-array)).
- **It is an update operator,** as `load_text_file` is, and reads the
  file when the program runs, relative to the directory it runs in,
  stopping it if the file can't be read (R0012), or isn't UTF-8 text
  (R0013). pandas' and R's `read_csv` are fixed to it, then to a call
  with the variable its `out` argument, and pandas' `usecols` and
  `names`, and Python's `has_header`, to `columns` and `header`.

```trendy,error
operator main{} [
    var rows: array tuple { city: text, people: integer };
    call load_csv{
        path: "cities.csv",
        header: true,
        columns: tuple { city: 1, people: 1 },
        out rows: rows,
    };
];
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
already holding its value: below, `ready` is simply `true`. So an initial
value can't read input, such as a file, which is known only when the
program runs: only an update operator reads input, which an expression
can't call (A0016).

```trendy
var ready := started or false;
var started := not stopped;
var stopped: boolean;

operator main{} [];
```

Operators can then give variables new values; see
[Assignment](#assignment).

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

### Local variables

An operator's body can declare variables of its own, written as the
program's are, which its statements use and assign as they do the
program's. They are purely local to the operator, as TTM allows even a
read-only one (RM Prescription 3), and gone once it returns.

```trendy
var visits := relation { city: text, guests: integer } {};

operator welcome{city: text, guests: integer} [
    var greeting: text;
    var arrival := relation { { city: city, guests: guests } } key { city };
    greeting := "karibu, " || city;
    visits := visits union arrival;
    call debug_value{v: greeting};
];

operator main{} [
    call welcome{city: "Mombasa", guests: 3};
    call welcome{city: "Kisumu", guests: 2};
    call debug_table{r: visits};
];
```

- **A local variable starts with its initial value,** or its type's
  default, as the program's do; but its value is computed when its
  declaration runs, from the parameters and variables then. A type with a
  type parameter in it has no default, so a local variable of one needs
  an initial value (A0006). A relation's keys are checked whenever it is
  assigned (R0008), and when compiling if the value is known then.
- **It is in scope in all of its block,** the body, or a branch of an
  `if` statement, so that its name means one thing there, and it hides a
  variable of the program's with its name before its declaration as well
  as after. But it has a value only from its declaration on, so a use
  before it is an error (A0045): as in Rust, a local variable can't be
  declared from a program's variable of its name, `var n := n + 1;`. A
  use after its block ends is an error too (A0015).
- **No two of an operator's names in scope are the same** (A0001): a
  local variable can't have a parameter's name, or that of another local
  variable in scope, in its block or one around it, though blocks side by
  side can each have one of a name. A second declaration of one, as Rust
  gives a variable a new value, is fixed to an assignment of the first.
- **A read-only operator's local variables name parts of its value,**
  each computed once, however many times it is used: below, `under`.
  Assigning one gives it a new value from there on, as in an update
  operator, and the rest of its value is still one expression, which sema
  evaluates when compiling where it can be.

```trendy
var dishes := relation {
    { dish: "jollof rice", price: 1392 },
    { dish: "ramen", price: 1044 },
    { dish: "pozole", price: 870 },
}
key { dish };

operator cheapest{
    limit: integer,
    dishes: relation { dish: text, price: integer },
}: tuple { dishes: integer, lowest: integer } [
    var under := dishes where price < limit;
    tuple { dishes: count{r: under}, lowest: min{r: under, of: price} }
];

operator discounted{price: integer, percent: integer}: integer [
    var discount := price * percent div 100;
    var paid := price - discount;
    paid := paid - paid mod 10;
    paid
];

operator main{} [
    call debug_value{v: cheapest{limit: 1100, dishes: dishes}};
    call debug_value{v: discounted{price: 1392, percent: 15}};
];
```

```trendy,error
var total := 0;

operator tally{n: integer} [
    total := total + n;
    var total := n * 2;
];

operator main{} [];
```

```trendy,error
operator scale{n: integer} [
    var n := n * 2;
    call debug_value{v: n};
];

operator main{} [];
```

### `if` statements

`if c then [ … ] else [ … ];` runs the statements of its first block if
the boolean `c` is true, and else those of its second, if it has an
`else`. Another condition is an `if` statement in the `else`, with one `;`
at the end of all: `if a then [ … ] else if b then [ … ] else [ … ];`.
Tutorial D's IF statement is the same (chapter 5), but for `END IF`.

```trendy
var stock := relation {
    { item: "teff", kilos: 12 },
    { item: "millet", kilos: 0 },
}
key { item };

operator restock{item_name: text, added: integer} [
    var held := stock where item = item_name;
    if is_empty{r: held} then [
        insert stock relation { { item: item_name, kilos: added } };
    ] else if added > 0 then [
        var note := "adding to " || item_name;
        update stock { kilos := kilos + added } where item = item_name;
        call debug_value{v: note};
    ] else [
        var note := "nothing to add to " || item_name;
        call debug_value{v: note};
    ];
];

operator main{} [
    call restock{item_name: "sorghum", added: 8};
    call restock{item_name: "millet", added: 5};
    call restock{item_name: "teff", added: 0};
    call debug_table{r: stock};
];
```

- **Each branch is a block in brackets,** even of one statement (P0029),
  so that a statement added to it can't end up outside it, and an `else`
  always belongs to the `if` it follows. A statement without brackets,
  Pascal's `begin … end` and Tutorial D's `BEGIN; … END` are fixed to
  brackets, and so is C's `{ … }` (P0003). The formatter puts each
  statement of a block on a line of its own, even of one.
- **A block's local variables are its own,** in scope in it alone, so two
  blocks side by side can each declare a `note`, as above.
- **Only in an update operator.** A read-only operator computes one value,
  so it chooses between values with an `if` expression (A0035).
- **A condition known when compiling runs one block only,** whose failures
  are the only ones reported. One written `true` or `false`, and an empty
  block, are errors, which the fix removes (A0046).
- **Other languages' spellings are fixed,** as an `if` expression's are
  (P0028): `elif` and its kin, and Tutorial D's `END IF`.

```trendy,error
operator report{n: integer} [
    if n = 0 then call debug_value{v: "none"};
];

operator main{} [];
```

```trendy,error
operator report{n: integer} [
    if n > 0 then [
        var note := "positive";
    ];
    call debug_value{v: note};
];

operator main{} [];
```

### Loops

A loop runs the statements of its block again and again while a boolean
condition is true. `while c loop [ … ];` checks the condition before each
run, so its block may never run, and `loop [ … ] while c;` checks it
after, so its block runs at least once. Tutorial D's WHILE loop is the
first (chapter 5). `for x in a loop [ … ];` runs its block for each
element of the array `a`, in order, with `x` the element's value.

```trendy
var queue := array ["Amara", "Kofi", "Wanjiru"];

operator serve{} [
    var i := 1;
    while i <= length{a: queue} loop [
        var next := queue[i];
        call debug_value{v: to_text{from: i} || ". " || next};
        i := i + 1;
    ];
    var tries := 0;
    loop [
        tries := tries + 1;
    ] while tries < 3;
    call debug_value{v: tries};
    for name in queue loop [
        call debug_value{v: "karibu, " || name};
    ];
];

operator main{} [
    call serve{};
];
```

- **A loop ends once its block makes its condition false.** No loop
  leaves early: trendy has no `break`, `continue` or Tutorial D's `LEAVE`
  (P0032), as it has no early `return`. A condition written `true`, or
  known when compiling to be true, never ends, and is an error, and so is
  one written `false`, and an empty block (A0046).
- **Its block is a block in brackets,** even of one statement, as an `if`
  statement's branches are (P0029), with the same fixes for other
  languages' blocks (P0003). Each run's local variables are its own,
  declared afresh, and out of scope after the loop, in its condition too.
- **Only in an update operator.** A read-only operator and a value block
  compute one value from whole relations, and change nothing that a loop
  could do again (A0035).
- **A `for` loop goes through an array** (A0022), computed once, before
  its first run. A relation's tuples have no order to go through them in
  (TTM RM Proscriptions 2 and 7), so an operator applies to a whole
  relation at once, as `sum` and `extend` do, or a load puts its tuples in
  an array first (see [Loading a relation](#loading-a-relation)).
- **Neither the loop's variable nor its array changes while it runs**
  (A0026): the block can't assign the variable, or a variable the array is
  computed from, and can't call an update operator that assigns one,
  directly or through the operators it calls. So a loop never goes past
  the end of an array that shrinks. To visit what a run adds, go through
  the positions with a `while` loop, which reads the array anew each time.
  An empty block does nothing, which the fix removes (A0046).
- **Other languages' spellings are fixed** (P0032): C's
  `while (c) { … }` and `do { … } while (c);`, Pascal's and Lua's
  `while c do … end` and `repeat … until c`, written
  `loop [ … ] while not (c);`, Tutorial D's `WHILE c; … END WHILE;`,
  C#'s `foreach`, and JavaScript's `for (x of a)`. C's
  `for (…; …; …)` is reported, as a `while` loop with a counter of its
  own.

```trendy,error
operator count_down{n: integer} [
    var i := n;
    while (i > 0) {
        i := i - 1;
    };
];

operator main{} [];
```

```trendy,error
operator count_down{n: integer} [
    var i := n;
    while true loop [
        i := i - 1;
    ];
];

operator main{} [];
```

```trendy,error
var queue := array ["Amara", "Kofi"];

operator serve{} [
    for name in queue loop [
        queue := array ["Wanjiru"];
    ];
];

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
IEEE 754 equality breaks that rule twice: NaN isn't equal to itself, and
`-0.0` is equal to `0.0` although `1/x` tells them apart. So approximate
numbers have neither:

- every approximate number is finite. No operator makes an infinity or a
  NaN, since a result out of range is an error, as an integer's is (see
  [Arithmetic](#arithmetic));
- there is one zero. Wherever IEEE 754 would make `-0.0`, the result is
  `0e0`, which is also how SQL databases have it: SQLite stores `-0.0` as
  `0.0`, and PostgreSQL compares the two equal.

So two approximate numbers are equal exactly when they are the same bits,
and they are ordered numerically.

Values never convert to another type implicitly, as *The Third Manifesto*
requires. An integer literal can't initialize an `approximate`: write `1e0`,
not `1`. Other values convert explicitly, with `to_approximate`,
`to_text`, `parse_integer`, `parse_approximate` and `parse_boolean` (see
[The prelude](#the-prelude)).

```trendy,error
var ratio: approximate := 1;

operator main{} [];
```

### A program's own types

A program can declare scalar types of its own, each with a possible
representation: the components a value of it can be written with (*The
Third Manifesto*, RM Prescription 4). A position on Earth is its latitude
and its longitude:

```trendy
/* A place on Earth, by its latitude and longitude, in degrees. */
type position { { latitude: approximate, longitude: approximate } };

heading city := { name: text, at: position };

var cities := relation city {
    { name: "Nairobi", at: position{latitude: -1.29e0, longitude: 36.82e0} },
    { name: "Lima", at: position{latitude: -12.05e0, longitude: -77.04e0} },
};

operator main{} [
    var nairobi := value from (cities where name = "Nairobi"){at};
    call debug_value{v: nairobi};
    call debug_value{v: nairobi.latitude};
    call debug_value{v: nairobi replace { latitude := 0e0 }};
    call debug_table{r: cities where at.latitude < -5e0};
];
```

- **The components go in braces of their own,** inside the type's:
  `type celsius { { degrees: approximate } };`, even for one. The outer
  braces hold what the declaration says of the type: its possible
  representations, its constraints and its order (see below). Their order
  doesn't matter to the compiler, but reads best that way: what the values
  are made of, which of those values the type has, and how they compare. Components written
  straight inside the type's braces, as a heading's attributes are, or
  after Tutorial D's `POSSREP`, are fixed to that (P0039).
- **The selector is named after the type,** and takes the components as
  its arguments: `position{latitude: -1.29e0, longitude: 36.82e0}` is the
  value of `position` with those components. It is an operator, as TTM's
  selectors are, and a call like any other, so a missing or misspelled
  component is reported as an argument is. No other operator can have a
  type's name, nor a type a prelude operator's (A0057).
- **`p.latitude` is a component's value,** with the dot of a tuple's
  attribute: TTM's `THE_LATITUDE(P)` (RM Prescription 5).
- **`p replace { latitude := x }` is the value with a component
  changed,** and the others as they were: shorthand for
  `position{latitude: x, longitude: p.longitude}`. Inside the braces, the
  components are in scope by name, and are computed from the old value,
  all at once, each keeping its type, as in a tuple's
  [`replace`](#substitution). Tutorial D has this expression for tuples
  and relations, and changes a scalar's components only in a variable,
  but TTM's case for those changes, that they don't depend on how the
  selector is written (chapter 3), holds for values too: `replace` names
  only the components that change.
- **`update p { latitude := x };` changes a component** of a variable's
  value: it is shorthand for `p := p replace { latitude := x };`, as
  Tutorial D's `UPDATE` of a scalar variable is (TTM chapter 5). In an
  `assign` block, `update`s of different components merge, and two of the
  same one conflict (R0009). `p.latitude := x;`, as other languages assign
  a field, is fixed to the `update`, with a `replace` for a value inside
  the variable's, as in `update stop { at := at replace { latitude := x } };`
  (A0048).
- **Values are equal when their components are,** and compared with `=`
  and `<>`; only a type with an order has `<` (see below), which is
  otherwise an error (A0022). A parameter of `any type` takes them, and
  one of `any ordered` those of a type with an order.
- **There is no default,** since no value of a type is any more a start
  than another: a variable of one, or of a tuple with one, needs an
  initial value (A0006).
- **A value is written as its selector's invocation,** by `debug_value`,
  `debug_table` and `to_text`: `position{latitude: -1.29e0, longitude:
  3.682e1}`, which reads back as the same value.
- **A type is scalar,** not a tuple: its components are its
  representation, not attributes, so nothing joins on them, and a value
  has the components its type declares, so none is projected away, added,
  removed or renamed (A0022). `tuple position { … }` is an error, fixed
  to the selector.
- **Types share a namespace with the headings,** and can be declared in
  any order. A component can be of any type, another of the program's
  own, a tuple or a relation, but not of its own type, directly or
  through others (A0049).

A type's constraints say which values it has: each is `constraint` and a
condition on the components, in the type's braces with its possible
representation, and a value of the type is one whose components satisfy
them all (*The Third Manifesto*, RM Prescription 23):

```trendy
type position {
    { latitude: approximate, longitude: approximate },
    constraint latitude >= -90e0 and latitude <= 90e0,
    constraint longitude > -180e0 and longitude <= 180e0,
};

operator north{p: position, by: approximate}: position [
    p replace { latitude := latitude + by }
];

operator main{} [
    var nairobi := position{latitude: -1.29e0, longitude: 36.82e0};
    call debug_value{v: north{p: nairobi, by: 10e0}};
];
```

- **A value is checked where it is selected,** by the selector,
  `replace` or `update`, or from a file's fields by `load_csv` (see [The
  prelude](#the-prelude)), the only ways a value of the type comes to be,
  as TTM has a selector check its type's constraint. Components that
  break a constraint fail there, so no variable ever holds a value
  outside its type. The failure names the value and the constraint it
  breaks: when compiling, if the components are known then, and else
  when running, as `north{p: nairobi, by: 100e0}` would (R0017).
- **Several constraints are one,** all of them true of every value, as
  TTM has one type constraint per type. Each is checked in order, so a
  failure says which one a value breaks.
- **A constraint is a condition on the components alone,** in scope by
  name. It reads no variable of the program's, whose value changes
  (A0050), and selects no value of its own type, directly or through
  operators, which would check it again without end (A0036). One known
  when compiling, true or false whatever the components, is an error
  (A0058).
- **The braces are a set,** so the constraints and the possible
  representation can come in any order. SQL's `check` is fixed to
  `constraint` (P0040), and a constraint inside the components' braces,
  where Tutorial D writes a possible representation's `CONSTRAINT`, is
  moved out of them (P0041).

A type's order says how its values compare: `order [asc year, …]`, in
its braces, compares them by the components it names, in turn, until one
differs, as a load's order compares tuples by their attributes, a `desc`
one the other way:

```trendy
type calendar_day {
    { year: integer, month: integer, day: integer },
    constraint month >= 1 and month <= 12,
    constraint day >= 1 and day <= 31,
    order [asc year, asc month, asc day],
};

var festivals := relation {
    { name: "Nowruz", on: calendar_day{year: 2026, month: 3, day: 20} },
    { name: "Diwali", on: calendar_day{year: 2026, month: 11, day: 8} },
    { name: "Lunar New Year", on: calendar_day{year: 2026, month: 2, day: 17} },
};

operator main{} [
    call debug_value{v: min{r: festivals, of: on}};
    call debug_table{r: festivals where on < calendar_day{year: 2026, month: 6, day: 1}};
];
```

- **A type with an order is ordered:** its values have `<`, `<=`, `>`
  and `>=`, and it is of the kind `ordered`, so `min`, `max`, a load's
  order and a type parameter of `any ordered` take them, as they take
  integers. Without one, `<` is an error, and so is a type where an
  ordered one is expected (A0022, A0004), each with help to give it an
  order.
- **The order names every component of one possible representation,
  once,** each of an ordered type (A0009, A0008, A0022), so that it is a
  total order consistent with `=`: values that differ are never equal in
  it. The compiler compares
  by it, so it can't be wrong, as a `<` the program wrote could be.
  Tutorial D's `ORDERED` and `ORDINAL`, which have the program write
  `<` (*Type Inheritance and Relational Theory*, chapter 2), are
  reported (P0042).
- **A type has one order** (A0008). It goes in the type's braces, not
  the components', as a constraint does (P0041).

A type can have more than one possible representation. Its values are
stored as the one without conversions, and each other converts to it,
with `to { stored := … }`, computing the stored components from its own,
and from it, with `from { own := … }`, computing its own from the stored
ones (*The Third Manifesto*, RM Prescription 4):

```trendy
type temperature {
    { kelvin: approximate },
    { celsius: approximate }
        to { kelvin := celsius + 273.15e0 }
        from { celsius := kelvin - 273.15e0 },
    { fahrenheit: approximate }
        to { kelvin := (fahrenheit - 32e0) * 5e0 / 9e0 + 273.15e0 }
        from { fahrenheit := (kelvin - 273.15e0) * 9e0 / 5e0 + 32e0 },
    constraint kelvin >= 0e0,
    order [asc kelvin],
};

operator main{} [
    var body := temperature{fahrenheit: 98.6e0};
    call debug_value{v: body};
    call debug_value{v: body.celsius};
    call debug_value{v: body > temperature{celsius: 30e0}};
    call debug_value{v: body replace { celsius := celsius + 1.5e0 }};
    update body { fahrenheit := 100.4e0 };
    call debug_value{v: body};
];
```

- **Each representation has a selector,** named after the type and told
  apart by its components' names, as operators of one name are:
  `temperature{celsius: 37e0}` is the value whose stored components its
  `to` computes. A value is written in its stored representation.
- **A component of any representation is read with the dot:**
  `body.celsius` is computed by its `from`, and a constraint can name
  one, as `constraint celsius < 100e0` would.
- **An order compares the components of any one representation,**
  converted to it by its `from` where values are compared:
  `order [desc fahrenheit]` would put the hottest first. The Third
  Manifesto's point, in `examples/inheritance.trendy`, is ordered by
  its polar coordinates, `order [asc r, asc theta]`, though stored by
  its cartesian ones. Components of two representations in one order
  are an error (A0059).
- **Each component is in one representation** (A0008), since its name
  says which, and each conversion computes each component it should,
  once, of its type (A0009, A0008, A0004), and reads no variable
  (A0050), so a value is the same in any representation.
- **Exactly one representation is stored,** and each other has `to` and
  `from`, once each (A0059, A0008). `to` and `from` the wrong way round
  are swapped by the fix.
- **`replace` and `update` change one representation's components:**
  the value is selected in the representation the components they name
  are of, from those, with the others it has there as they were, as TTM's
  `THE_X ( P ) := 2.0` is shorthand for `P := CARTESIAN ( 2.0, THE_Y ( P
  ) )` (chapter 3). So `body replace { celsius := celsius + 1.5e0 }` is
  `temperature{celsius: body.celsius + 1.5e0}`. Components of two
  representations in one change are an error (A0059), since no selector
  takes them together.
- **Every representation's components are in scope** in the values of a
  `replace` or an `update`, as in a constraint, each but the stored ones
  computed by its `from`: `body replace { kelvin := celsius }` is fine.
- **An `update` in another representation than the stored one gives
  every stored component a new value,** so in an `assign` block it
  conflicts with any other change to the variable (R0009).
- **`load_csv` reads a value in any representation,** from the columns
  `columns` gives its components, as in `mean: tuple { celsius: 3 }`,
  converting it to the stored one (see [The
  prelude](#the-prelude)).

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
- A trailing comma after the last attribute is allowed, and the formatter
  writes one where the tuple is laid out one attribute per line (see
  [Layout](#layout)).
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
  declared type), or from a tuple whose type is known on its own, in that
  order. So an empty relation needs one of the first two. Which tuple
  gives it doesn't depend on their order, which they don't have: in
  `relation { { r: relation {} }, { r: relation { { b: 1 } } } }`, the
  empty relation takes its heading from the second tuple's.
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

## Headings

A heading is the attributes of a tuple or a relation, with their types,
such as `{ city: text, rooms: integer }`. `heading name := { … };`
declares one with a name, and the name stands for the attributes wherever
a heading follows `relation` or `tuple`: in types, and in selectors.

```trendy
heading home := { city: text, rooms: integer };
heading street := { name: text, homes: relation home };

var homes: relation home := relation home {
    { city: "Lima", rooms: 3 },
    { city: "Pune", rooms: 5 },
};
var high_street := tuple street { name: "Jalan Besar", homes: homes };

operator largest{homes: relation home}: tuple home [
    tuple from (homes where rooms = max{r: homes, of: rooms})
];

operator main{} [
    call debug_value{v: largest{homes: high_street.homes}};
];
```

- **It names attributes, not a type.** A heading is the attributes of
  tuples and relations alike, so it has two types, `relation home` and
  `tuple home`, which are the types its attributes written out make:
  `relation home` is `relation { city: text, rooms: integer }`. In *The
  Third Manifesto*, a tuple type's name is, precisely, `TUPLE H`, where
  `H` is its heading (RM Prescriptions 6 and 7), and two such types are
  equal when their headings are, so a value fits wherever its attributes
  do, named or not. Values print with their attributes written out.
- **Declared at the top level, in any order,** as operators are: one
  declared in an operator's body is moved out (P0034). A heading's
  attributes can be of other headings' types, but it can't contain
  itself, directly or through others (A0049): TTM leaves open whether a
  heading can, and assumes it can't (chapter 6, RM Prescription 6).
- **Its attributes have specific types,** as a variable's do: not
  `any type`, nor an operator's type parameter (A0014, A0003).
- **Headings share a namespace with the types,** so no two have the same
  name, nor a heading a type's (A0001), but a heading can have a
  variable's, an operator's or an attribute's name. `heading` is a keyword
  only before a heading's name, so an attribute can be called `heading`,
  as a ship's is.
- **A heading's name alone is not a type** (A0003), as a type of records'
  is in other languages: `{h: home}` is fixed to `relation home` or
  `tuple home`. Their declarations are fixed to a heading's (P0004):
  TypeScript's `type Home = { … }` and `interface Home { … }`, and C's and
  Rust's `struct Home { … }`. So is `heading home := relation { … }`
  (P0033).

```trendy,error
heading home := { city: text, rooms: integer };

operator describe{h: home} [];

operator main{} [];
```

```trendy,error
interface Home {
    city: text,
    rooms: integer,
}

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
- **Every value must satisfy the keys.** Two tuples with the same values
  for a key are an error (R0008). An initial value is checked when
  compiling, and the error is reported at the two tuples; an assigned
  value is checked when the assignment is made, or, in an `assign` block,
  once all of the block's are (see [Assignment](#assignment)), and a value
  known only when the program runs stops it, writing the variable, the
  key and the two tuples.
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

## Arrays

An array is a list of values of one type, its elements, in order. An
array type is written `array type`, and an array value
`array [element, …]`, with the elements' type first where there is
nothing else to give it, as in `array text []`. Arrays are values like
any other: a tuple's attribute can be an array, and so can a relation's,
or an array's element. *The Third Manifesto* describes `ARRAY` as a type
generator, whose types can be used wherever a type can, an attribute's
too, with a selector, subscripts, assignment and equality (chapter 3);
Tutorial D keeps arrays special, as only the target of `LOAD`, but a
fully orthogonal language would make them first-class (chapter 5), as
trendy does.

```trendy
var rainfall := array [1200, 950, 220];
var none: array text;
var cities := relation {
    { city: "Cairo", rainy: array [] },
    { city: "Mumbai", rainy: array ["Jun", "Jul", "Aug", "Sep"] },
};

operator first{a: array E}: E
    types { E: any type }
[
    a[1]
];

operator main{} [
    call debug_value{v: rainfall[2]};
    call debug_value{v: length{a: rainfall}};
    call debug_value{v: first{a: array ["Amara", "Kofi"]}};
    call debug_table{r: cities};
];
```

- **Positions count from 1.** `a[i]` is the element of `a` at the
  position `i`, an integer from 1 up to `length{a: a}`, as in SQL and
  Tutorial D. Any other position is an error (R0011): when compiling, if
  the array's length and the position are known then, as a position below
  1 always is, and otherwise when the program runs, which stops it. A `[`
  right after a value always indexes it.
- **The elements' type comes from context,** as a relation's heading
  does: from the selector's own, from the type expected of the value, or
  from an element whose type is known on its own, in that order. So an
  empty array needs one of the first two (A0011), which another tuple of
  a relation can give, as Mumbai's does Cairo's above.
- **The default is the empty array.** An array variable declared without
  a value starts with no elements.
- **Equality is element by element.** Two arrays are equal when they have
  the same length and equal elements at each position, so
  `array [1, 2] <> array [2, 1]`. Arrays have no order among themselves,
  so `<`, `<=`, `>` and `>=` don't apply to them (A0022).
- **Elements can repeat,** as a relation's tuples can't: an array is a
  list, not a set.
- **A relation is not an array.** A relation's tuples have no order (RM
  Proscription 2), so a relation has no positions, and indexing it is an
  error (A0022). A load puts its tuples into an array, in an order (see
  [Loading a relation](#loading-a-relation)).
- **Other languages' habits are fixed:** a list written `[1, 2]`, as
  Python and JavaScript write it, to `array [1, 2]` (P0002), and SQL's
  `ARRAY[1, 2]` too (P0010); `a[0]` for the first element, to `a[1]`, and
  `a[-1]` for the last, to `a[length{a: a}]` (R0011); `len`, `size` and
  `CARDINALITY`, to `length` of an array and `count` of a relation, and
  `count` and `length` given the other's kind of value to each other
  (A0015, A0004); and `t["name"]` for a tuple's attribute, to `t.name`
  (A0022).

```trendy,error
var unknown := array [];

operator main{} [];
```

```trendy,error
var rainfall := array [1200, 950, 220];
var first := rainfall[0];

operator main{} [];
```

```trendy,error
var cities := relation { { city: "Cairo" }, { city: "Mumbai" } };
var first := cities[1];

operator main{} [];
```

### Changing an element

`a[i] := x;` gives the element of the array variable `a` at position `i`
the value `x`. It is shorthand for assigning `a` the array with that
element changed and the others as they were: *The Third Manifesto* calls
such a target a pseudovariable, and any assignment to one shorthand for an
assignment to the variable (RM Prescription 5). Pseudovariables nest, so
`grid[i][j] := x;` changes an element of an element.

```trendy
var rainfall := array [1200, 950, 220];
var grid := array [array [1, 2], array [3, 4]];

operator main{} [
    var before := rainfall;
    rainfall[3] := 250;
    grid[2][1] := 30;
    call debug_value{v: rainfall};
    call debug_value{v: before};
    call debug_value{v: grid};
];
```

- **Arrays are values,** so changing one changes no other: `before` above
  keeps the old elements, where other languages' variables share a list.
  A change copies nothing when nothing else holds the array, which it then
  changes in place, and copies the array otherwise.
- **The position is one the array has,** as for reading one: any other is
  an error (R0011), and `a[0]` and `a[-1]` are fixed as they are there. An
  assignment never adds an element: give the variable a new array for
  that, as in `a := array [1, 2, 3];`.
- **A tuple's attributes are no variables to assign,** as Tutorial D's
  aren't, so `t.name := x;` is fixed to `update t { name := x };`, and an
  attribute of an array's element, `a[i].name := x;`, to
  `a[i] := a[i] replace { name := x };` (A0048).
- **In an `assign` block, changes to an array's elements merge,** each
  computed from the array as it was (see
  [Assigning at once](#assigning-at-once)), so
  `assign { a[1] := a[2]; a[2] := a[1]; };` swaps two elements. Two that
  set the same element, or one an element in the other's, conflict
  (R0009): when their positions are known only when running, they stop the
  program if they are the same then.
- **An array a `for` loop goes through can't be changed in it** (A0026):
  go through its positions with a `while` loop instead.
- **A value block, or a read-only operator's body, can change its own
  local variable's elements,** and so compute an array with an element
  changed, as a value: `[ var b := a; b[i] := x; b ]`.

```trendy,error
var people := array [{ name: "Wangari", born: 1940 }];

operator main{} [
    people[1].name := "Wangari Maathai";
];
```

```trendy,error
var rainfall := array [1200, 950, 220];

operator main{} [
    assign {
        rainfall[1] := 0;
        rainfall[1] := 1;
    };
];
```

### Loading a relation

`load a from r order [asc x, desc y];` gives the array variable `a` the
tuples of the relation `r`, in order: by `x`, ascending, then, among
tuples with equal values of `x`, by `y`, descending. It is Tutorial D's
`LOAD a FROM r ORDER (ASC x, DESC y)` (chapter 5). `load a value from r
order [asc x];` gives `a` the values of a relation of one attribute
instead, such as a projection's, as `value from r` is the value of a
relation's only tuple's only attribute. `remove {x}` after the order
leaves attributes out of the elements, once the tuples are ordered, and
`keep {x}` leaves out all the others.

```trendy
var yearly := relation {
    { city: "Mumbai", rain: 2200 },
    { city: "Cairo", rain: 25 },
    { city: "Lagos", rain: 1500 },
}
key { city };

operator main{} [
    var ranked: array tuple { city: text, rain: integer };
    load ranked from yearly order [desc rain, asc city];
    var names: array text;
    load names value from yearly{city} order [asc city];
    var rains: array integer;
    load rains value from yearly order [asc city] remove {city};
    var wettest: array text;
    load wettest value from yearly order [desc rain] keep {city};
    call debug_value{v: ranked[1].city};
    call debug_value{v: names};
    call debug_value{v: rains};
    call debug_value{v: wettest};
];
```

- **The order is undefined unless it is total.** Tuples equal on every
  item ordered by come out in an order the language doesn't define, which
  a program must not depend on. An order is total when its items include
  a key of the relation, as `city` is above; all of its attributes always
  do, and so does the one attribute of a relation of values.
- **A load is an assignment,** of the whole array, and not written as one,
  since that would convert a relation into an array implicitly, which
  values never do (TTM chapter 5). So it can be in an `assign` block, and
  assign a read-only operator's local variable. There is no expression
  that orders a relation, which would make one a list (RM Proscription
  7): an array of its tuples is a value of another type.
- **Each item names its direction,** `asc` or `desc`, before an
  attribute of the relation (P0030, A0010), of a type with an order:
  booleans, integers, approximate numbers and text (A0022). The order
  names at least one attribute (A0029), each once (A0008).
- **The load says what the array takes,** and its elements must be that
  (A0004, A0047): its tuples, of their heading, which an empty relation
  can take from them; or, with `value`, the values of its one attribute,
  of their type, which can be tuples too. An array of tuples and an array
  of their values are different types, as a relation of one attribute and
  that attribute's type are, so nothing converts one into the other
  unless the load says so; one into an array of other values without
  `value` gets it from the fix, as does a relation of several attributes
  the order names one of, its projection on that one.
- **`remove` after the order leaves attributes out of the elements,**
  once the tuples are ordered by them, as `rains` leaves out `city`, and
  `keep` leaves out all those it doesn't name, as `wettest` keeps only
  `city`: whichever is shorter to write. A load has one of the two at
  most (P0030). Each element is still there, though some turn out equal:
  they are an array's elements, not a relation's tuples. Removing
  attributes from the relation instead, `load a from r remove {x} order
  […]`, leaves them out before the order, which then can't name them, and
  merges tuples that turn out equal; ordering by one is reported, with a
  fix that removes it after the order (A0010). Each attribute named is
  one of the relation's, named once (A0010, A0008). An array with fewer
  attributes than the relation, or of the values of one, is fixed to
  leave the others out, with whichever of `remove` and `keep` names fewer
  (A0004, A0047). `keep` is a contextual keyword, and other languages'
  words are fixed (P0030): `drop` to `remove`, and SQL's `select`, the
  algebra's `project`, and braces alone, as a projection is written, to
  `keep`.
- **`load`, `value`, `from`, `order`, `asc`, `desc` and `keep` are
  contextual keywords:** they are names elsewhere, so an attribute can be
  called `order` or `desc`.
- **Other languages' habits are fixed** (P0030): SQL's `ORDER BY x DESC`
  to `order [desc x]`, its `NULLS FIRST` and `NULLS LAST` deleted, an item
  without a direction given `asc`, LINQ's `descending` written `desc`,
  Tutorial D's capitals and parentheses, and an ordering written as an
  assignment's value, `a := r order [asc x];`, to a load.

```trendy,error
var yearly := relation { { city: "Mumbai", rain: 2200 } };

operator main{} [
    var names: array text;
    load names from yearly ORDER BY city DESC;
];
```

```trendy,error
var yearly := relation { { city: "Mumbai", rain: 2200 } };

operator main{} [
    var names: array text;
    load names from yearly order [asc city];
];
```

### Loading an array

`load r from a as x;` gives the relation variable `r` a tuple for each
element of the array `a`, whose attribute `x` is the element. `numbered
n` adds `n`, the element's position, counting from 1, and `where c`
keeps the tuples for which `c` is true, with their attributes, the
position's too, in scope. It is the converse of loading a relation, as
Tutorial D's `LOAD r FROM a` is of its `LOAD a FROM r` (chapter 5), so
`load lines from texts as line numbered n;` and `load texts value from
lines order [asc n] remove {n};` give each other back.

```trendy
operator main{} [
    var texts: array text;
    call load_text_file{path: "rivers.txt", out lines: texts};
    var lines: relation { line: text, n: integer };
    load lines from texts as line numbered n where n > 1 and line <> "";
    var rivers: relation { river: text };
    load rivers from texts as river where river <> "";
    var flows := array [tuple { river: "Nile", km: 6650 }, tuple { river: "Niger", km: 4180 }];
    var longest: relation { river: text, km: integer, rank: integer };
    load longest from flows numbered rank;
    call debug_table{r: lines};
    call debug_table{r: rivers};
    call debug_table{r: longest};
];
```

- **Each element makes a tuple:** an array of tuples' elements are
  theirs, and any other element is the value of the attribute `as x`
  names, which an array of tuples can name too, to make a relation of
  one tuple-valued attribute. Without `as`, an array of other values is
  reported (A0047), and so is a relation whose heading isn't the tuples'
  (A0047), each with a fix that writes `as` and `numbered` as its heading
  needs, when only one way fits it.
- **Elements that make equal tuples make one,** as a projection's
  do, since a relation's tuples are a set: `rivers` above has the Nile
  once. Tutorial D requires an array with no duplicates instead. With
  `numbered`, no two tuples are equal, as no two elements have one
  position, so each element is there, as in `lines`. The position's
  attribute is one the tuples haven't (A0024).
- **`where` comes after `as` and `numbered`,** and sees what they name,
  so `where n > 1` skips a header line. Without `numbered`, a name
  `where` doesn't know is reported with help to write it (A0015), unless
  it may be a misspelling of one it does. A load of a relation into an
  array can have one too, before its order: `load a from r where c order
  […]`.
- **A load is an assignment** of the whole relation, written as a load
  for the same reason as a load into an array: nothing converts an array
  into a relation implicitly. A relation's tuples have no order, so a
  load into one has none, nor `value`, which are for a load into an
  array, as `as` and `numbered` are for a load into a relation (A0047);
  each is fixed away. A relation loaded into a relation variable is fixed
  to an assignment (A0047).
- **`numbered` is a contextual keyword,** and Postgres's `WITH
  ORDINALITY AS n` is fixed to `numbered n` (P0030).

```trendy,error
operator main{} [
    var texts := array ["Nile", "Niger"];
    var rivers: relation { river: text };
    load rivers from texts;
];
```

```trendy,error
operator main{} [
    var texts := array ["Nile", "Niger"];
    var lines: relation { line: text, n: integer };
    load lines from texts as line with ordinality as n;
];
```

## Assignment

An operator's body gives a variable a new value with an assignment
statement, `name := value;`, or a shorthand for one (*The Third Manifesto*,
RM Prescription 21).

```trendy
var poets := relation {
    { name: "Ada", born: 1815 },
    { name: "Bashō", born: 1644 },
}
key { name };

var address := tuple { city: "Nairobi", postal_code: "00100" };
var visits := 0;

operator main{} [
    visits := visits + 1;
    insert poets relation { { name: "Rumi", born: 1207 } };
    delete poets where born < 1300;
    update poets { born := born + 1 } where name = "Ada";
    update address { postal_code := "00200" };
];
```

- **The value has the variable's type,** which never changes: an
  assignment of another type is an error (A0004). `=` is equality, not
  assignment (P0005).
- **The new value is written in full,** as `visits := visits + 1`: there
  is no `+=`, `++` or `--`, which are fixed to the assignment they mean
  (P0036).
- **Only variables are assigned,** and an array variable's elements (see
  [Changing an element](#changing-an-element)), and an update operator's
  `var` and `out` parameters, which assign the variables they are passed
  (see [`var` and `out` parameters](#var-and-out-parameters)). Its other
  parameters are `const`, and an operator is not a variable (A0026).
- **The shorthands are for relation variables,** but for `update`, which
  is also for tuple variables and those of a program's own types, and an
  element's assignment, for array variables. Each means an assignment:

| Statement | Means |
|---|---|
| `insert r s;` | `r := r union s;` |
| `d_insert r s;` | `r := r d_union s;`, which fails if a tuple of `s` is already in `r` (R0006) |
| `delete r where p;` | `r := r where not (p);` |
| `i_delete r s;` | `r := r i_minus s;`, which fails if a tuple of `s` isn't in `r` (R0007) |
| `update r { a := x };` | `r := r replace { a := x };`, of a relation, a tuple or a value of a type of the program's own (see [A program's own types](#a-programs-own-types)) |
| `update r { a := x } where p;` | `r := (r where not (p)) union ((r where p) replace { a := x });` |
| `load a from r order [asc x];` | `a :=` the array of the tuples of `r`, in order (see [Loading a relation](#loading-a-relation)) |
| `a[i] := x;` | `a :=` the array `a` with its element at position `i` changed to `x` (see [Changing an element](#changing-an-element)) |

- **`delete` needs `where` and a condition** (P0023), so that every tuple
  isn't deleted by accident. To delete them all, assign the empty relation:
  `r := relation {};`, whose heading comes from the variable.
- **An `update`'s new values come before `where`:**
  `update r { a := x } where p;`, not as Tutorial D orders them (P0024).
  Its condition and values have the attributes of the tuple at hand in
  scope, as in `where` and `replace`.
- **A change of every tuple says so:** `r := relation {};` deletes them
  all, and `update r { a := x };`, without `where`, updates them all. A
  condition known when compiling to be true whatever the tuple, such as
  `true` or `1 = 1`, selects them all another way, as does
  `i_delete r r;`, and is an error (A0032).
- **An assignment must change something.** Giving a variable its own
  value, `x := x;`, or an attribute its own, `update r { a := a };`, is an
  error, and so is inserting or deleting the empty relation,
  `insert r relation {};`, a condition known when compiling to be false,
  `delete r where false;`, or inserting a variable into itself,
  `insert r r;`, unless other changes in its block remove tuples for it to
  add back (A0033). So are an `update` of no attributes,
  `update r {};`, and a block of no assignments, `assign {};` (A0023).

```trendy,error
var poets := relation { { name: "Ada", born: 1815 } };

operator main{} [
    delete poets;
];
```

```trendy,error
var poets := relation { { name: "Ada", born: 1815 } };

operator main{} [
    delete poets where true;
    update poets { born := born + 1 } where 1 = 1;
];
```

```trendy,error
var poets := relation { { name: "Ada", born: 1815 } };

operator main{} [
    update poets { name := name, born := born + 1 };
];
```

```trendy,error
var visits := 0;

operator record{visits: integer} [
    visits := visits + 1;
];

operator main{} [];
```

### Assigning at once

An `assign` block makes several assignments at once, as one statement
(RM Prescription 21's multiple assignment). Its braces say that the order
of its assignments doesn't matter: each ends with `;`, as a statement does,
and the block with `};`.

```trendy
var x := 1;
var y := 2;
var rooms := relation {
    { room: 1, guest: "Amara" },
    { room: 2, guest: "Kenji" },
}
key { room }
key { guest };

operator main{} [
    assign {
        x := y;
        y := x;
    };
    assign {
        update rooms { guest := "Kenji" } where room = 1;
        update rooms { guest := "Amara" } where room = 2;
    };
];
```

- **Every value is computed from the variables as they were before the
  block,** so `x := y; y := x;` in a block swaps them, whereas as two
  statements both would end up `y`'s.
- **Changes to one variable merge into its new value.** An `insert`,
  `d_insert`, `delete`, `i_delete` or `update` of a relation is a change to
  its tuples: the new value is the old one less every tuple the changes
  remove, which a `delete` or an `update` selects and an `i_delete` names,
  with every tuple they add. Each condition sees the old tuples, so the two
  `update`s above swap the guests. An `update` of a tuple changes its
  attributes, and `update`s of different attributes merge, as do
  assignments of an array's different elements.
- **Changes that touch the same tuple, attribute or element conflict,**
  since neither order would be the right one. Two changes that remove the
  same tuple stop the program (R0009), as do two `update`s of the same
  attribute of a tuple, two assignments of the same element of an array,
  or of an element and an element in it, and `v := …` with any other
  change to `v`; each is an error when compiling when it is known then. This program compiles,
  but stops when it runs, writing `error[R0009]: two changes in an assign
  block remove the same tuple: tuple { guest: "Kenji", room: 1 }`, since
  both changes remove Kenji's:

```trendy
var rooms := relation { { room: 1, guest: "Kenji" } } key { room };

operator main{} [
    assign {
        update rooms { guest := "Wanjiru" } where room = 1;
        delete rooms where guest = "Kenji";
    };
];
```

- **`d_insert` checks the tuples it adds against those the block keeps,**
  and the other `d_insert`s' in it, and `i_delete` checks the tuples it
  names against the old value.
- **Keys are checked once all the block's assignments are made,** so a key
  can be broken between them: as two statements, the first `update` above
  would give Kenji two rooms. A variable whose new value has two tuples
  with the same values for a key stops the program (R0008); when the value
  is known while compiling, that is an error then.
- **Only assignments go in a block.** Several assignments separated by
  commas, as Tutorial D writes them, are fixed to a block (P0025).

```trendy,error
var x := 1;
var y := 2;

operator main{} [
    x := y, y := x;
];
```

```trendy,error
var count := 0;

operator main{} [
    assign {
        count := count + 1;
        count := count + 2;
    };
];
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
| `and{a, b, c}` | `true` when every one is `true` |
| `or{a, b, c}` | `true` when one is `true` |
| `xor{a, b, c}` | `true` when an odd number are `true` |

The operands must be booleans: there is no truthiness, so `not 0` and
`"" or true` are errors.

- **Lists:** `and{…}`, `or{…}` and `xor{…}` take any number of
  operands, in braces, since their order doesn't matter, as Tutorial D's
  `AND {…}`, `OR {…}` and `XOR {…}` do (TTM, chapter 5): `and{a, b, c}`
  is `a and b and c`. Of one operand, each is that operand, and of none,
  `and{}` is `true`, and `or{}` and `xor{}` are `false`, which leave a
  result as the other operands make it. An operand written twice counts
  once in `and{…}` and `or{…}`, but twice in `xor{…}`, so that it is the
  chain of `xor`s, as TTM's exercise 5.6 justifies its definition, though
  Tutorial D's braces would count it once: `xor{a, a}` is `false`. The
  operands have no names, as a call's arguments do (P0037).
- **An operand that can't fail guards those that can.** In a chain of
  `and`s, `a and b and c`, however it is grouped, or `and{a, b, c}`, the
  operands that can't fail are computed first, and if one is `false`,
  the result is `false` and the others aren't computed; otherwise every
  one that can fail is. `or` is the same with `true`, and `implies` is
  `not a or b`. So `y div x > 1 and x <> 0` never divides by zero,
  wherever the guard is written, unlike in languages that guard only from
  the left, or SQL, which doesn't say. As nothing depends on which
  operand runs first, an optimizer can choose.
- **An expression can fail** if it does arithmetic, `+`, `-`, `*`, `/`,
  `div`, `mod` or a prefix `-` of an integer, which can overflow or divide
  by zero; joins texts, `||`, or builds, changes, combines or loads a
  relation or an array, which can run out of memory; takes an element or
  the only tuple; or calls an operator. Comparisons, the boolean
  operators, literals, names, attributes and tuple selectors can't fail
  themselves; an expression made of them can fail only if a part of it
  can.

```trendy
var points := relation { { x: 0, y: 5 }, { x: 2, y: 6 } };
var steep := points where y div x > 1 and x <> 0;

operator main{} [
    call debug_table{r: steep};
];
```

```trendy
var a := true;
var b := not a or a and false;
var c := (a or b) and not b;
var d := a implies b implies c;
var e := or{a, b, not c};

operator check{p: boolean, q: boolean} [
    call debug_value{v: p xor q};
    call debug_value{v: not (p and q)};
    call debug_value{v: xor{p, q, true}};
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
must be written `1e0` and `2e0`, and otherwise from the left operand. An
integer that isn't a literal converts with `to_approximate`, as in
`to_approximate{from: n} * 1.5e0`. `/` doesn't divide integers, and `div`
doesn't divide approximate numbers (A0022). `+` and `*` also apply to
values of a type parameter of `any numeric` (see [Kinds](#kinds)), as the
type a call gives it.

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
`0.1e0 + 0.2e0` is `0.30000000000000004e0`. There is one zero, though, so
`0e0 * -1e0` is `0e0` (see [Types](#types)).

An operator whose result is out of range has no result: an integer outside 64
bits, or an approximate number beyond about `1.8e308` in magnitude. Neither
does division by zero. Results never wrap around and never become infinities
or NaN, so every approximate number is finite and can be written as a
literal. When the operands are known while compiling,
this is a compile error (R0001 for overflow, R0002 for division by zero).
Otherwise the program stops at that operator, writing the error to stderr,
with the operation and its operands' values, such as `error[R0001]:
integer overflow: 9223372036854775807 + 1 doesn't fit in a 64-bit
integer`, and where it was (see [Programs](#programs)), and exits with
status 1.

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
the smallest integer, `-9223372036854775808`, can be written.

The prelude's `sqrt`, `sin` and `cos` give an approximate number's square
root, and the sine and cosine of an angle in radians, and `atan2` the angle
from the positive x axis to the point `(x, y)`, from -π, exclusive, to π.
`pi{}` is the approximate number nearest π: a constant is a read-only
operator without parameters, called as any other. `sqrt` is correctly
rounded, as IEEE 754 requires; the others are computed the same way on
every machine, so a result is the same whether the compiler computes it or
the program does. A negative number has no square root (R0018), which is
a compile error when it is known while compiling, and otherwise stops the
program as an overflow does.

```trendy
var circumference := 2e0 * pi{} * 1.5e0;
var hypotenuse := sqrt{of: 3e0 * 3e0 + 4e0 * 4e0};

operator polar{x: approximate, y: approximate} [
    call debug_value{v: sqrt{of: x * x + y * y}};
    call debug_value{v: atan2{y: y, x: x}};
    call debug_value{v: cos{of: pi{} / 3e0}};
];

operator main{} [
    call polar{x: 1e0, y: 1e0};
];
```

```trendy,error
var root := sqrt{of: -4e0};

operator main{} [];
```

### Text

`a || b` is the text `a` followed by the text `b`. Both operands must be
text: `"n = " || 1` is an error, since values never convert implicitly, and
`+` doesn't concatenate (A0022, with a fix to `||`). `to_text` converts a
value to text, as in `"n = " || to_text{from: n}`.

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
| `approximate` | numerical |
| `text` | by code point, which is also the order of the UTF-8 bytes. There is no collation: `"Z" < "a"`, and `"é" > "z"` |

Two values are equal exactly when they are the same value, as *The Third
Manifesto* requires (RM Prescription 8). Two tuples are equal when every
attribute's values are equal, and two relations when they have the same
tuples, in any order; the values inside are compared the same way. Two
arrays are equal when they have equal elements at each position. Tuples and
arrays have no order, so `<`, `<=`, `>` and `>=` don't apply to them
(A0022).

Relations are ordered as sets, by inclusion, as in Tutorial D: `r <= s`
is true when every tuple of `r` is in `s` (subset), `r < s` when, too,
`s` has more (proper subset), and `r >= s` and `r > s` the same the
other way around (superset and proper superset). The order is partial:
for relations that each have a tuple the other lacks, all four are false.

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
    call debug_value{v: relation { { x: 1 } } <= r};
    call debug_value{v: r < relation { { x: 1 } }};
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

### `if`

`if c then a else b` is `a` if the boolean `c` is true, and else `b`.
Another condition is an `if` in the `else` branch:
`if a then x else if b then y else z`. *The Third Manifesto* assumes
IF-THEN-ELSE expressions of the usual form (chapter 5).

```trendy
var dishes := relation {
    { dish: "jollof rice", price: 1200 },
    { dish: "ramen", price: 900 },
};
var label := if count{r: dishes} = 1 then "dish" else "dishes";

operator mean{r: relation { dish: text, price: integer }}: integer [
    if is_empty{r: r} then 0 else sum{r: r, of: price} div count{r: r}
];

operator sign{n: integer}: text [
    if n < 0 then "negative" else if n = 0 then "zero" else "positive"
];

operator main{} [
    call debug_value{v: mean{r: dishes}};
    call debug_value{v: mean{r: dishes where price > 5000}};
    call debug_value{v: sign{n: -3}};
];
```

- **Every `if` has an `else`,** since it has a value either way (P0002).
- **The branches have one type,** the type expected of the `if`, or else
  that of a branch whose type is known on its own, whichever it is, so
  `if c then relation {} else r` takes its heading from `r`. The condition
  is a boolean (A0004).
- **Only the branch taken is computed.** So `mean` above never divides by
  zero, and a failure in the other branch never happens. Where the
  condition is known when compiling, the `if` is the branch it takes, and
  the other's failures aren't reported; otherwise a branch that is known
  to fail is an error, since it fails whenever it is taken.
- **An `if` that decides nothing is an error** (A0046): a condition written
  `true` or `false`, which the fix replaces with the branch taken, and the
  branches `true` and `false`, which are the condition itself, or `not` it.
  A condition that is only known when compiling, from variables, is
  fine.
- **The `else` branch takes all that follows,** as `where`'s condition
  does, so `if c then a else b + 1` adds 1 to `b`. As an operand of
  another operator, an `if` needs parentheses to say where it ends:
  `(if c then 2 else 3) + 4` (P0022).
- **Other languages' spellings are fixed** (P0028): Python's
  `a if c else b`, C's `c ? a : b`, `elif`, `elsif` and `elseif` for
  `else if`, and Tutorial D's `END IF`, which an `if` doesn't have.

```trendy,error
operator label{n: integer}: text [
    if n = 1 then "one"
];

operator main{} [];
```

```trendy,error
operator label{n: integer}: text [
    if n = 1 then "one" else 2
];

operator main{} [];
```

```trendy,error
operator positive{n: integer}: boolean [
    if n > 0 then true else false
];

operator main{} [];
```

```trendy,error
var plus := 1 + if true then 2 else 3;

operator main{} [];
```

```trendy,error
operator label{n: integer}: text [
    "one" if n = 1 else "many"
];

operator main{} [];
```

### Value blocks

A value block computes a value in steps, where the value goes: local
variables declared and assigned, then the value, an expression without
`;`. It is a read-only operator's body in place, and Tutorial D's
`WITH … : exp` (TTM chapter 5), which introduces names in turn, then
gives the value.

```trendy
var people := relation {
    { name: "Ada", born: 1815 },
    { name: "Alan", born: 1912 },
};
var span := [
    var first := min{r: people, of: born};
    var last := max{r: people, of: born};
    last - first
];

operator label{n: integer}: text [
    if n < 100 then [
        var short := 100 - n;
        "short by " || to_text{from: short}
    ] else [
        var over := n - 100;
        "over by " || to_text{from: over}
    ]
];

operator main{} [
    call debug_value{v: span};
    call debug_value{v: label{n: span}};
    call debug_table{r: people extend { age_at_50 := [ var at := born + 50; at - born ] }};
];
```

- **It goes wherever a value does:** in an initial value, a body, an
  argument, an `if`'s branches, or an `extend`'s values, where the tuple's
  attributes are in scope, as above.
- **It has a statement before its value,** which a `var` starts or a `;`
  ends; brackets with no statement are an array's habit (see
  [Arrays](#arrays)). One that doesn't end with a value is an error
  (P0031), and so are braces, as Rust and C write a block, fixed to
  brackets (P0003).
- **Its local variables are its own.** They are in scope in all of it,
  but have no value before their declarations, and none after it. They
  hide no name around it, a parameter's, a local variable's or an
  attribute of the tuple at hand (A0001), but they hide a variable of the
  program's.
- **It changes nothing else,** as a read-only operator doesn't: it can't
  assign a variable that isn't its own, `call` an operator (A0034), or
  have an `if` statement or a `return` (A0035). It chooses between values
  with an `if` expression, whose branches can be value blocks; an `if`
  that runs to the end of a body or block, with no `;`, is its value.
- **It is evaluated when compiling** where its values are known, as any
  expression is.

```trendy,error
operator twice{n: integer}: integer [
    [
        var n := 2;
        n * n
    ]
];

operator main{} [];
```

```trendy,error
var total := 0;

operator main{} [
    call debug_value{v: [
        var a := 1;
        total := a;
        a
    ]};
];
```

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
    call debug_table{r: names};
];
```

- **The result is a set.** Projecting a relation can make tuples equal,
  and duplicates are dropped: `names` above has one tuple.
- **The names must be the operand's attributes,** each named once. An
  unknown name gets a fix renaming it to the closest attribute.
- **`r{}` keeps nothing:** a tuple becomes `tuple {}`, and a relation
  becomes `relation {} { {} }` (table_dee) if it has tuples, or
  `relation {} {}` (table_dum) if not. `remove {}` removes nothing, and is
  an error (A0023). A name right before `{}` is an operator if there is
  one with that name (see [Names](#names-1)), so `(r){}` projects a value
  that has an operator's name.
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
    call debug_table{r: totals};
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
`name := value`, and of a value of a type of the program's own, the value
with components given new values (see
[A program's own types](#a-programs-own-types)). The values are computed from the tuple's own attributes,
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
    call debug_table{r: doubled};
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
  `replace {}`, which replaces nothing (A0023), and an attribute given its
  own value, `a := a`, which changes nothing (A0033).
- **Tuples that become equal are one,** since a relation is a set: the
  result can have fewer tuples than the operand.
- Assignments are written as in `extend`, and the same habits are fixed
  (P0011). `replace` is not an update: it changes no variable, and leaves
  the operand as it was. The `update` statement assigns a variable its
  `replace` (see [Assignment](#assignment)).

```trendy,error
var items := relation { { name: "tea", price: 3 } };
var doubled := items replace { price := price * 2e0 };

operator main{} [];
```

### Image relations

In the values of an `extend` or a `replace`, a `where`'s condition, or
an argument for a `per` parameter, `!!r` is the image relation of the
tuple at hand in the relation `r`: the tuples of `r` that
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
var listed := countries extend { has_cities := not is_empty{r: !!cities} };

operator main{} [
    call debug_table{r: by_country};
];
```

- **It is a join, then a remove:** `r` joined with the tuple at hand,
  projected on their common attributes, then without them. So, as in
  `join`, the relation must have an attribute in common with the tuple at
  hand (A0028), with the same type (A0027).
- **A tuple with no match has an empty image,** so every tuple at hand
  gets one: above, Peru's `cities` is empty, and so its `has_cities` is
  false.
  This is grouping without SQL's GROUP BY, which loses the empty groups;
  with aggregate operators, it will also be how to summarize.
- **The tuple at hand is the innermost one:** in a nested `extend`, the
  inner tuple's. There is none outside an `extend`'s or a `replace`'s
  values, a `where`'s condition and a `per` argument, so `!!` there is an
  error (A0030).
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
    call debug_table{r: by_place};
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
    call debug_table{r: memberships};
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
    call debug_table{r: big_french};
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
- **A join or a product is restricted as it is made.** When compiling,
  each part of the condition that `and` joins is computed only for the
  values the operands have of the attributes it reads, so
  `times{r, s, t} where a = b and c < 2` never makes the whole product,
  and a product of many relations can be restricted all at once, as the
  conditions read, rather than a join at a time. When running, each part
  that can't fail restricts the join as soon as it has the attributes the
  part reads, and operands related by a common attribute, or by a part
  that reads both, are joined before anything is multiplied. Either way,
  the parts that can't fail are computed first; the others, for the
  tuples those keep.
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
- **It takes a relation's expression,** as Tutorial D's does: its
  relational operators, from the unions to the joins, and a `where` with
  its condition, so `tuple from r where p` is of `r where p`. Postfix
  operators bind tighter, so `value from r{country}` takes from
  `r{country}`; to project after a `where`, parenthesize it, as above.
  What no relation has ends it: a comparison, so `tuple from r = t`
  compares the tuple, and arithmetic, so `value from r{born} + 1` adds to
  the value, as in Tutorial D, whose `<relation exp>` has neither (TTM,
  Appendix I).
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
    call debug_table{r: placed};
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
    call debug_table{r: placed};
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

### Matching

`r matching s` is the tuples of `r` that have a match in `s`, on their
common attributes, and `r not matching s` those that have none: the
semijoin and the antijoin. The result has `r`'s attributes only. They
are Tutorial D's `MATCHING` and `NOT MATCHING`.

```trendy
var people := relation {
    { name: "Ada", city: "London" },
    { name: "Wangari", city: "Nyeri" },
};
var kenyan := relation { { city: "Nyeri", country: "Kenya" } };
var in_kenya := people matching kenyan;
var elsewhere := people not matching kenyan;
var names_elsewhere := (people not matching kenyan) remove {city};

operator main{} [
    call debug_table{r: elsewhere};
];
```

- **Written like the joins,** with their precedence, and their rules for
  common attributes: at least one (A0028), of the same type (A0027). They
  take relations only (A0022), and have no list form, since the order of
  the operands matters.
- **To match several relations, chain them:** `r matching s matching t`
  is the tuples of `r` with a match in `s` and a match in `t`, since the
  joins group to the left. That differs from `r matching (s join t)`,
  which needs one tuple of the join of `s` and `t` to match.
- **To drop the common attributes,** remove them, as `names_elsewhere`
  does; for `matching`, `r compose s{a}` is the same, with `s` projected on
  the attributes to match.
- The literature's `semijoin`, `semiminus` and `antijoin` are fixed to
  them (P0014), and so are Tutorial D's capitals.

```trendy,error
var people := relation { { name: "Ada", city: "London" } };
var kenyan := relation { { city: "Nyeri" } };
var elsewhere := people antijoin kenyan;

operator main{} [];
```

### Union, intersection and difference

`r union s` is the tuples of either relation, `r intersect s` the tuples
of both, `r minus s` the tuples of `r` that aren't in `s`, and
`r xunion s` the tuples of exactly one, as `xor` is of booleans. The
relations must have the same heading, so an empty `relation {}` takes the
other's, whichever side it is on. They are Tutorial D's `UNION`,
`INTERSECT` and `MINUS`, and SQL's `UNION`, `INTERSECT` and `EXCEPT`.

```trendy
var born := relation { { name: "Ada" }, { name: "Édouard" } };
var listed := relation { { name: "Ada" }, { name: "Wangari" } };
var anyone := born union listed;
var both := born intersect listed;
var unlisted := born minus listed;
var in_one := born xunion listed;
var everyone := union{born, listed, relation { { name: "Edgar" } }};
var only_born := minus[born, listed, relation { { name: "Edgar" } }];
var partition := (born minus listed) d_union listed;
var checked := born i_minus (born intersect listed);

operator main{} [
    call debug_table{r: anyone};
];
```

- **One heading:** the same attributes, of the same types (A0031). When
  each operand has one attribute the other lacks, of one type, the fix
  renames it. They take relations; `union` of tuples, Tutorial D's tuple
  union, is fixed to `join`.
- **Precedence:** `intersect` and `minus` are ANDs in Algebra A and bind
  like the joins, while `union` is an OR and binds more loosely, as `or`
  does than `and`: `r union s join t` is `r union (s join t)`. `xunion`
  binds as `union` does. `xunion{r, s, t}` is the tuples of an odd number
  of them, as `xor{a, b, c}` is true of an odd number of operands. SQL reads
  set operators from the left, so `r union s minus t`, which the algebra
  would read as `r union (s minus t)`, needs parentheses (P0022).
- **Lists:** `union{…}`, `xunion{…}` and `intersect{…}` in braces, since
  their operands can be in any order, and `minus[…]` in brackets, from the left:
  `minus[r, s, t]` is `(r minus s) minus t`. None has a value with no
  operands (A0029).
- **Checked forms:** `r d_union s` is a union of operands with no tuple in
  common, and `r i_minus s` a minus whose right operand's tuples are all
  in the left. Each states what the plain form would quietly accept, and
  fails if it doesn't hold: when compiling if the relations are known
  then, naming the tuple, and otherwise when the program runs (R0006,
  R0007). They bind and list as `union` and `minus` do: `d_union{…}`,
  `i_minus[…]`.
- SQL's `except` is fixed to `minus`, and `union all`'s `all` is removed,
  since a relation has no duplicates (P0021).

```trendy,error
var born := relation { { name: "Ada" } };
var listed := relation { { name: "Wangari" } };
var unlisted := born except listed;

operator main{} [];
```

### Precedence

From tightest to loosest:

| Operators | Grouping |
|---|---|
| `!!`, of a name or a parenthesized expression | none: see below |
| postfix `.a`, indexing `[…]`, projection `{…}`, `remove {…}`, `rename {…}`, `extend {…}`, `replace {…}`, `group {…}`, `ungroup {…}`, `wrap {…}` and `unwrap {…}` | left |
| prefix `not`, `-`, `+`, `tuple from` and `value from` | |
| `*`, `/`, `div`, `mod` | left |
| `+`, `-`, `\|\|` | left |
| `join`, `times`, `compose`, `matching`, `not matching`, `intersect`, `minus`, `i_minus` | left |
| `union`, `d_union`, `xunion` | left |
| `=`, `<>`, `<`, `<=`, `>`, `>=` | none: they don't chain |
| `and` | left |
| `xor` | left |
| `or` | left |
| `implies` | right |
| `where`, whose condition is all that follows | left |
| `if … then … else …`, whose `else` branch is all that follows | none: an operand needs parentheses |

So `-a * b` is `(-a) * b`, `a + 1 < b` is `(a + 1) < b`, and, as with
every prefix operator, `not a = b` is `(not a) = b`. To negate a
comparison, parenthesize it: `not (n = 0)`. Writing `not n = 0` for an
integer `n` is an error, since `not` takes a boolean, and its fix is that
parenthesization; SQL and Python read it that way.
`implies` groups from the right, as in logic: `a implies b implies c`
means `a implies (b implies c)`. The joins bind looser than arithmetic,
since their operands are relations, and tighter than the comparisons, so
`r join s = t` compares the join. `tuple from` and `value from` take the
relational operators after them, from the unions to the joins, and a
`where` with its condition, but not arithmetic or a comparison, so
`value from r{a} + 1` is `(value from r{a}) + 1` (see
[Taking the only tuple](#taking-the-only-tuple)). Parentheses group
explicitly.

Precedence is a partial order: where a pair of operators could reasonably
be read either way, neither reading is assumed, and parentheses must say
which is meant. These pairs are:

- **A comparison and another comparison,** since `a < b < c` doesn't
  chain (P0015).
- **An `if` as an operand,** since its `else` branch takes all that
  follows: `1 + if c then 2 else 3 + 4` could add the `if` or end it at
  `3`. Write `1 + (if c then 2 else 3 + 4)` or `1 + (if c then 2 else 3) + 4`
  (P0022).
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
one is never skipped, but for an `if`'s branches, only the one its
condition takes, and for the operands of `and`, `or` and `implies` that
can fail, which those that can't guard (see [Boolean
operators](#boolean-operators)). Whatever doesn't depend on parameters,
variables or input is evaluated when compiling, and so is whatever a constant operand decides: `p
and false` is just `false`, whatever `p` is, `true and p` is just `p`, and
`n * 1` is just `n`. But `(n + 1) * 0` is still computed, since `n + 1` can
overflow. A failure in an operand a guard known when compiling skips isn't
reported, as one in an `if`'s branch not taken isn't; one known to fail
behind a guard known only when the program runs is an error, since it
fails whenever it is computed. Approximate numbers are simplified the same way, since
they are finite and have one zero: `x + 0e0` is `x`, and `x * 0e0` is `0e0`.

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
`tuple`, `relation`, `array`, `true`, `false`, `not`, `and`, `or`, `xor`,
`implies`, `div`, `mod`, `remove`, `rename`, `extend`, `replace`, `group`,
`ungroup`, `wrap`, `unwrap`, `join`, `times`, `compose`, `matching`,
`union`, `d_union`, `xunion`, `intersect`, `minus`, `i_minus`, `where`,
`as`, `insert`, `d_insert`, `delete`, `i_delete`, `update`, `assign`,
`return`, `if`, `then`, `else`, `while`, `loop` and `for`, all lowercase,
and a keyword can't be a name (P0017).
`type` is a keyword only after `builtin` and before a type's name, `key`
only where a variable's keys go, `value` and `from` only in `tuple from` and `value from`, and
`load`, `from`, `order`, `asc` and `desc` only in a load, `in` only
in a `for` loop, and `heading` only before a heading's name.

Operators, values, and types with headings each have a namespace of
their own. Values are variables and parameters, and a parameter hides a
variable of its name. So a variable can have an operator's name, or a
type's, and which one a name is depends on where it is: in
`call greet{…};` it is an operator, and as a value it is a variable or a
parameter. No two operators can have the same name, nor two variables,
nor two headings, nor a heading and a type (A0001), and a type's name is
its selector's, which no other operator can have (A0057).

```trendy
var greet := "Hello";
var text: text := "allowed";

operator greet{name: text} [
    call debug_value{v: greet || ", " || name};
];

operator main{} [
    call greet{name: "Amara"};
];
```

```trendy,error
var main: integer := 0;
var main := 1;

operator main{} [];
```

- **A name right before `{}` is an operator** if there is one with that
  name, even if a value has it too, as the name after `call` is. In an
  expression, `greet{}` is then a call of `greet`, which needs a value,
  and an update operator has none: it is invoked only by a `call`
  statement (A0016). To project a value of the same name on no
  attributes, parenthesize it: `(poets){}`. The parentheses are needed
  only then; with attributes, `poets{name}` can't be a call, and is a
  projection.

```trendy,error
var poets := relation { { name: "Ada" } };

operator poets{} [];

operator main{} [
    call debug_table{r: poets{}};
];
```

## Layout

`trendyfmt` defines the canonical layout:

- one definition per line;
- consecutive variable declarations on adjacent lines, unless you separate
  them with a blank line to group them, and so for headings'; several
  blank lines become one;
- a blank line between other definitions;
- single spaces around `:=` and after `:`;
- lines are at most 100 characters where a list, an `if` expression or
  a chain of operators can be broken: a line that would be longer has its
  outermost list, such as an operator's parameters, a call's arguments, a
  heading or a tuple value, written one item per line, its outermost
  `if`'s `then` and `else` each starting a line, one level in, or its
  outermost chain of operators of one precedence, such as a `where` or
  the `and`s of a condition, with each operator starting a line, one
  level in, until it fits; an `else if` chain stays at its `if`'s level,
  and a list goes before a chain beside it:

  ```
  var later := cities
      where people_in_2000 > 2 * people_in_1900 + 1000000
          and country <> "France"
          and country <> "Ghana";
  ```

- a block's items go one level in from the line it opens on, and its
  closing delimiter back at that line's level; a chain's operators, or an
  `if`'s `then` and `else`, go one level in from the line the chain or
  `if` starts on, or, when that is the first line of another, one level
  in from that one's, so the `and`s above aren't mistaken for the
  `where`'s, nor a broken comparison in the first `and`'s operand for the
  `and`s;
- an `if` statement's blocks, as an operator's body, have each statement
  on its own line, even one alone;
- a relation value with more than one tuple has each tuple on its own line,
  indented four spaces per level of nesting;
- a tuple value, relation value or call's arguments with several items, one
  of which spans lines, has each item on its own line too, so nothing
  follows a value's last line but its closing braces; a single item stays
  inside its braces, as in `call f{v: relation {` … `}};`, unless a chain
  of operators broken across lines is in it;
- a relation's tuples list their attributes in the order of the relation's
  heading: the selector's own heading, else the declared type, else the first
  tuple's order, where a heading's name stands for its declaration's;
- an operator's body has each statement on its own line, indented, on
  adjacent lines unless you separate them with a blank line to group them;
  several blank lines become one, and a read-only operator's result type
  goes right after its parameters: `operator f{x: integer}: integer [`;
- an operator's type parameters go on a line of their own, indented, with
  spaces inside their braces, and then its body's `[` on a line of its own,
  in the operator's column:

  ```
  operator same{a: T, b: T}: boolean
      types { T: any type }
  [
      a = b
  ];
  ```

- an `assign` block has each assignment on its own line, indented, like
  an operator's body;
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

A list laid out one item per line has a comma after its last item, so
that adding an item changes no other line, and an inline list has none:
the formatter adds or drops it, as the layout says.

Comments are kept exactly as written. A comment on a line of its own stays
on its own line, indented like the code after it, where the layout breaks
the line anyway; elsewhere it stays in the line, between the same code. A
comment after code on the same line stays there, after one space. A blank
line after a comment on its own line, such as a file's header, is kept.

## Coming from other languages

| Elsewhere | In trendy |
|---|---|
| `fn`, `def`, `function`, `void main()` | `operator main{} [];` |
| `fn f(x: i64) -> i64 { x + 1 }`, `RETURNS INTEGER` | `operator f{x: integer}: integer [x + 1];` |
| `return x;`, `RETURN x` at the end of a body | `x` |
| `{ let x = 1; x * 2 }`, Rust's block, in an expression | `[ var x := 1; x * 2 ]` |
| `while (c) { … }`, `while c do … end`, `WHILE c; … END WHILE` | `while c loop [ … ];` |
| `do { … } while (c);`, `repeat … until c` | `loop [ … ] while c;`, `loop [ … ] while not (c);` |
| `break`, `continue`, `LEAVE` | none: a loop ends when its condition is false |
| `foreach (x in a) { … }`, `for x of a`, `for x in a:` | `for x in a loop [ … ];` |
| `for (i = 0; i < n; i++)` | `var i := 1; while i <= n loop [ …; i := i + 1; ];` |
| `print(x)`, `f(x)` | `call debug_value{v: x};` |
| `x + 1;`, an expression as a statement | `x := x + 1;` to keep its value, `call debug_value{v: x + 1};` to see it |
| `f(x)` in an expression | `f{x: x}`, every argument named |
| `let`, `const`, `int x` | `var x`, at the top level or in an operator's body |
| `let x = x + 1;`, declaring a variable again | `x := x + 1;`, assigning it |
| `x = 0` | `x := 0` |
| `x += 1`, `x++`, `++x` | `x := x + 1;` |
| `INSERT R …`, `INSERT INTO r …` | `insert r …;` |
| `DELETE R WHERE P`, `DELETE FROM r WHERE p` | `delete r where p;` |
| `DELETE R`, `DELETE FROM r`, `… WHERE 1 = 1` | `r := relation {};` |
| `UPDATE R WHERE P ( A := X )`, `UPDATE r SET a = x WHERE p` | `update r { a := x } where p;` |
| `UPDATE r SET a = x`, `… WHERE 1 = 1` | `update r { a := x };` |
| `X := Y, Y := X`, a multiple assignment | `assign { x := y; y := x; };` |
| `(params) { body }` | `{params} [body]` |
| `ref int n`, `n: inout Int`, `n : in out Integer`, `UPDATES { N }` | `var n: integer`, before the name |
| `out int n`, `n : out Integer` | `out n: integer`, before the name |
| `f(ref x)`, `f(&x)`, `f(&mut x)`, `f(x)` for Delphi's `var` parameter | `call f{var n: x};` |
| `f(out x)`, `f(x)` for Delphi's `out` parameter | `call f{out n: x};` |
| `fn same<T>(a: T, b: T)`, `same<T>(a: T, b: T)` | `operator same{a: T, b: T} types { T: any type }` |
| `any`, `anyelement` | `any type` |
| `SUM (SP, QTY)`, an attribute for each tuple | `of: integer per r`, an expression of each tuple's attributes |
| `COUNT(*)`, `COUNT (R)`, `len(r)`, `r.count()` | `count{r: r}` |
| `GROUP BY a` with `COUNT(*)` | `groups extend { n := count{r: !!r} }`, which keeps the empty groups |
| `MIN(x)`, `MAX (R, X)` | `min{r: r, of: x}`, `max{r: r, of: x}` |
| `SUM(x)`, `SUM (R, X)`, `sum(xs)` | `sum{r: r, of: x}`, which is 0 of no tuples, not `NULL` |
| `EXP(SUM(LN(x)))`, `math.prod(xs)` | `product{r: r, of: x}`, exact, of any numbers, and 1 of no tuples |
| `AVG(x)`, `mean(xs)` | `sum{r: r, of: x} / to_approximate{from: count{r: r}}` |
| `GROUP BY a` with `MAX(x)` | `(groups matching r) extend { m := max{r: !!r, of: x} }`, since an empty group has no max |
| `bool` | `boolean` |
| `True`, `TRUE`, `1` for true | `true` |
| `int`, `long`, `i64` | `integer` |
| `float`, `double`, `real`, `f64` | `approximate` |
| `0.5`, `.5`, `5.` | `0.5e0`, `5e0` |
| `string`, `str`, `varchar` | `text` |
| `str(x)`, `x.to_string()`, `CAST(x AS VARCHAR)`, `CAST_AS_CHAR (X)` | `to_text{from: x}` |
| `float(n)`, `n as f64`, `CAST(n AS DOUBLE PRECISION)`, `CAST_AS_RATIONAL (N)` | `to_approximate{from: n}` |
| `int(s)`, `parseInt(s)`, `atoi(s)`, `CAST(s AS INTEGER)`, `CAST_AS_INTEGER (S)` | `parse_integer{from: s}` |
| `float(s)`, `parseFloat(s)`, `atof(s)`, `CAST(s AS DOUBLE PRECISION)` | `parse_approximate{from: s}` |
| `Boolean.parseBoolean(s)`, `strconv.ParseBool(s)`, `CAST(s AS BOOLEAN)` | `parse_boolean{from: s}` |
| `lines = readLines(path)`, `File.readlines(path)`, `File.ReadAllLines(path)` | `call load_text_file{path: path, out lines: lines};`, an array of the lines |
| `rows = pd.read_csv(path, usecols=[0, 2])`, `read_csv(path)` | `call load_csv{path: path, header: true, columns: tuple { city: 1, people: 3 }, out rows: rows};`, an array of tuples whose columns the call names, from 1 |
| `{ x: 1 }`, `TUPLE { X 1 }` | `tuple { x: 1 }` |
| `type Home = { … }`, `interface Home { … }`, `struct Home { … }` | `heading home := { … };`, whose types are `relation home` and `tuple home` |
| `[1, 2]`, `ARRAY[1, 2]` | `array [1, 2]` |
| `[]`, an empty list | `array integer []`, with its elements' type, or `array []` where it is known |
| `a[0]`, the first element | `a[1]` |
| `a[-1]`, the last element | `a[length{a: a}]` |
| `len(a)`, `a.length()`, `CARDINALITY(a)`, `size(a)` | `length{a: a}` |
| `ORDER BY x DESC`, `LOAD A FROM R ORDER (DESC X)`, `orderby x descending` | `load a from r order [desc x];` |
| `unnest(a) WITH ORDINALITY AS t(x, n)`, `enumerate(a)`, `LOAD R FROM A` | `load r from a as x numbered n;` |
| `a := r order [asc x];` | `load a from r order [asc x];` |
| `NULLS FIRST`, `NULLS LAST` | nothing: there are no nulls |
| `tuple { x = 1 }` | `tuple { x: 1 }` |
| `RELATION { TUPLE { … } }` | `relation { { … } }` |
| `VAR`, `TUPLE`, `TRUE` | `var`, `tuple`, `true` |
| `!a`, `a && b` | `not a`, `a and b` |
| `a \|\| b` for booleans | `a or b` |
| `"a" + "b"`, `concat("a", "b")` | `"a" \|\| "b"` |
| `a ^ b`, `a != b` for booleans | `a xor b` |
| `a -> b`, `a => b` | `a implies b` |
| `AND { a, b, c }`, Python's `all([a, b, c])` | `and{a, b, c}` |
| Python's `any([a, b, c])` | `or{a, b, c}` |
| `AND`, `Or`, `NOT` | `and`, `or`, `not` |
| `a == b`, `a != b` | `a = b`, `a <> b` |
| `a < b < c` | `a < b and b < c` |
| `a if c else b`, `c ? a : b`, `IF c THEN a ELSE b END IF` | `if c then a else b` |
| `elif`, `elsif`, `ELSEIF` | `else if` |
| `if (c) { … }`, `IF c THEN BEGIN; … END; END IF`, `if c then … ;` | `if c then [ … ];` |
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
| `R UNION S`, `UNION { R, S, T }` | `r union s`, `union{r, s, t}` |
| `R INTERSECT S`, SQL's `INTERSECT` | `r intersect s` |
| `R MINUS S`, SQL's `EXCEPT` | `r minus s` |
| `UNION ALL` | `union`: a relation has no duplicates |
| `R D_UNION S`, `R I_MINUS S` | `r d_union s`, `r i_minus s` |
| symmetric difference, Python's `s ^ t` | `r xunion s` |
| `R1 ⊆ R2`, `R1 ⊂ R2`, `R1 ⊇ R2`, `R1 ⊃ R2`, Python's `s <= t` | `r1 <= r2`, `r1 < r2`, `r1 >= r2`, `r1 > r2` |
| `R MATCHING S`, `R SEMIJOIN S`, `WHERE EXISTS (…)` | `r matching s` |
| `R NOT MATCHING S`, `R SEMIMINUS S`, `WHERE NOT EXISTS (…)` | `r not matching s` |
| `T1 UNION T2`, of tuples (tuple union) | `t1 join t2` |
| `JOIN … ON r.a = s.b` | `r join s rename {b as a}` |
| `LEFT JOIN`, `RIGHT JOIN`, `FULL JOIN` | none: there are no nulls |
| `a / b` for integers | `a div b` |
| `a % b` | `a mod b` |
| `1 / 2` meaning one half | `1e0 / 2e0` |
| `PI`, `M_PI`, `math.pi`, `Math.PI` | `pi{}`, a call of an operator without parameters |
| `sine(x)`, `cosine(x)`, `arctan2(y, x)` | `sin{of: x}`, `cos{of: x}`, `atan2{y: y, x: x}` |
| `tan(x)`, `atan(x)`, `hypot(x, y)`, `abs(x)` | `sin{of: x} / cos{of: x}`, `atan2{y: x, x: 1e0}`, `sqrt{of: x * x + y * y}`, `if x < 0e0 then -x else x` |
| `inf`, `NaN`, `1e0 / 0e0` | an error: approximate numbers are finite |

The compiler recognizes each of these habits and suggests the fix.
`trendyc --explain <code>` describes any error code in detail, and
`trendyc --error-format=json` prints each error as JSON, with its fixes as
exact edits.
