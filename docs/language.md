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
then where the program was, then the calls the failure passed through,
innermost first, each as a note naming the operator and where it was
called, and exits with status 1. Where it was is the statement
running, or, in a value block, such as a read-only operator's body, the
statement that gives a local variable the value being computed, or the
value the block ends with. Here, a recipe in `projects/shopping` that
serves no one, by its file's line, divides by zero where `shopping_list`
scales each ingredient to the people a meal is cooked for, which `main`
calls:

```text
error[R0002]: division by zero: 2e1 / 0e0 has no result
 --> projects/shopping/shopping.trendy:90:5
  = note: in `shopping_list`, called at projects/shopping/shopping.trendy:183:5
```

The calls aren't part of the error: an error is what went wrong, a value
the program can test (see [Failures](#failures)), and the calls are
where its failure went.

What the program is started with, its arguments and its environment, are
the variables of `trendy.process`, a unit that comes with the compiler (see
[Arguments and the environment](#arguments-and-the-environment)), which `main`
reads, as any other unit can.

`main` can give errors, as its parameter `out errors`, of the type a
transaction collects into, `relation transaction_collection` (see
[Transactions](#transactions)), which it assigns, as any `out`
parameter (A0054). When it returns, the program writes them: the
warnings, those whose `failed` is false, as `warning[code]: message`,
then the failures, as `error[code]: message`, each with its kind's code
and message, and no site, as an error holds none. It exits with 1 if
one failed, and else with 0. A failure nothing takes still stops the
program where it fails. Here the records a load skips are worth
knowing, but not failures:

```trendy
uses { trendy.files };

operator main{out errors: relation transaction_collection} [
    var rows: array tuple { city: text, people: integer };
    var skipped: relation { error: error };
    call load_csv{
        path: file_path{text: "cities.csv"},
        header: true,
        columns: tuple { city: 1, people: 2 },
        out rows: rows,
        out errors: skipped,
    };
    errors := skipped extend { failed := false };
];
```

`main` takes no other parameters, and has no result type (A0019): the
fixes make `errors` an `out` parameter, and of its type, and remove
another, such as `arguments`, `args` or `argv`, which they read from
`trendy.process` instead.

## Units

A file is a *unit*. Its *interface* declares what other units see of it,
and its *implementation* what they don't. `interface` and
`implementation`, each alone on its line, start them, and each holds the
declarations after it, up to the next. What comes before either is the
implementation's, so a file without them, as each program so far is,
exports nothing.

```trendy
interface

/* A depth, in whole metres. */
type metres {
    { m: integer },
    constraint m > 0,
};

heading lake := { name: text, native: text, depth: metres };

/* The lakes recorded so far. */
var known: relation lake key { name };

operator deepest{lakes: relation lake}: relation lake;

operator record{name: text, native: text, depth: integer};

implementation

operator deepest{lakes: relation lake}: relation lake [
    lakes where depth.m = max{r: lakes, of: depth.m}
];

operator record{name: text, native: text, depth: integer} [
    insert known relation { { name: name, native: native, depth: metres{m: depth} } };
];

operator main{} [];
```

- **A unit has a use:** `main`, where a program starts, or an interface that
  declares something for other units to see, or both, as a library can
  carry a small program of its own. With neither, nothing can run it or use
  it, which is an error (A0002), in a unit that is used, or under a database's
  directory, too. `trendyc` makes a program, so it reports a unit without
  `main` however good its interface. `trendyc --check` checks either kind and
  makes nothing, as the language server does.
- **The interface declares types, headings, operators and variables,
  each operator by its heading alone.** The implementation gives each such
  operator's body, repeating its whole heading, its parameters with their
  modes and types, and its result type, as the interface declares them
  (A0068). A heading with a body nowhere is a specification, whose
  versions give its bodies (see [Union types](#union-types)).
- **A variable the interface declares is other units' too.** They read and
  assign it as the unit does, as the program below `insert`s into
  `known`: by its name, or qualified, `lakes::known`, and pass it to `var`
  and `out` parameters, and add a transaction's errors to it. So it is
  declared there whole, with its initial value, which is no secret (A0001,
  whose fix moves the implementation's value there). What holds of it is
  said by the constraints on it, which are the interface's, where every
  unit that assigns it sees them (A0067, whose fix moves one there), and
  no other unit's (A0083; see [Constraints](#constraints)). A read-only
  operator reads it no more than any other variable (A0050). The
  implementation's variables are its own, which other units reach only
  through its update operators. A constant is its interface's to declare,
  as the prelude's `pi` is, with its value or only its type (see
  [Constants](#constants)).

```trendy,unit lakes
interface

heading lake := { name: text, depth: integer };

var known: relation lake key { name };

/* Every lake recorded is deeper than its surface. */
constraint deep := is_empty{r: known where depth <= 0};
```

```trendy
uses { lakes };

operator main{} [
    insert known relation { { name: "Titicaca", depth: 281 } };
    lakes::known := lakes::known union relation { { name: "Baikal", depth: 1642 } };
    call debug_value{v: count{r: known}};
];
```

- **Versions go in the implementation** (A0067): a call names a
  specification, and dispatch finds the version for its arguments'
  types, whatever unit calls it, so there is nothing about a version to
  export.
- **The interface sees only interfaces, and the implementation sees both** (A0067):
  a name written in the interface, in an operator's heading, a heading's attributes,
  a type's components, supertype and constraints, a constant's value, a variable's
  type and initial value, or a constraint's condition, means what this unit's
  interface, the prelude's or that of a unit its `uses` names declares. What the
  interface reads is what other units read, so nothing in it is a name they can't
  look up, and a condition it states is one they can compute themselves. The fix
  declares it there: an operator by its heading, leaving its body, and a type, a
  heading, a constant or a variable whole, though a constraint on the
  implementation's variables alone moves to the implementation instead, which
  keeps them its own. An operator's body and a type's conversions are in the
  implementation, so they can use anything, and what is only in their way stays
  private. A unit's own implementation hides nothing from its interface: a name
  only the implementation declares, that the prelude also does, means the prelude's
  there.
- **A type the interface declares is declared with its possible
  representations,** its logical interface (TTM RM Prescription 4): the
  interface writes their components, its constraints, a representation's
  own among them, and its order, which say what its values are, so that
  other units select them, read their components and compare them, and
  its constraints hold whoever selects one. How its representations
  convert to each other is how its values are stored, which is the
  unit's own (see below).

A type with several possible representations the interface declares is
declared again in the implementation, which gives every one but the
stored one its `to` and `from`, so that no other unit can tell which of
them the values are stored as:

```trendy
interface

/* The time something takes, to the second. */
type duration {
    { seconds: integer, constraint seconds >= 0 },
    { hours: integer, minutes: integer, constraint minutes >= 0 and minutes < 60 },
};

implementation

type duration {
    { hours: integer, minutes: integer }
        to { seconds := hours * 3600 + minutes * 60 }
        from { hours := seconds div 3600, minutes := seconds mod 3600 div 60 },
};

operator main{} [
    var flight := duration{hours: 7, minutes: 45};
    call debug_value{v: flight.minutes};
];
```

- **The implementation's declaration completes the interface's,** with
  nothing else: what the type is, its constraints and its order, is
  written once, in the interface, and a constraint or an order in the
  implementation's declaration is moved there (A0067). Conversions in the
  interface are moved to the implementation's, which the fix writes if
  it is missing (A0067).
- **It repeats only what it extends:** the components of each
  representation it gives conversions, as the interface writes them, with
  their types, as an operator's body repeats its heading, so that each
  conversion reads beside what it converts. One the interface doesn't
  declare is an error, and components that differ in their types are fixed
  to the interface's (A0068). The stored one, which it has nothing to add
  to, it can leave out, as here, or repeat, with no conversions.
- **Exactly one is left without conversions,** so leaving out two is an
  error, and so is leaving out none (A0059), as a type's values are stored
  as one of its representations.
- **A type with one possible representation needs no second
  declaration,** as it has nothing to convert, and one with several
  declared in an interface without one is an error (A0059).
- **A unit's names are its own.** A name written in a unit is one it
  declares, or one of the prelude's interface, which every unit sees, so
  two units can each declare one of a name, which is no duplicate
  (A0001), and a unit's own hides the prelude's, or a used unit's, of its
  name, so that a unit never qualifies its own names. The program calls a
  declaration by its name if one unit declares it, and otherwise by its
  name qualified by its unit's, `lakes::metres`, in every unit that
  declares it, `main`'s and the prelude's too, as errors, failures and
  `debug_value` write it: a program's own `count{r: …}` is
  `main::count`, and the prelude's `trendy.prelude::count`. Another unit that
  comes with the compiler counts only if a unit of the program uses it, so a
  program's own `date` is `date`, unless it uses `trendy.time`. Only the
  built-in types' names are no unit's to declare.
- **A unit has one interface and one implementation, the interface
  first** (P0053). `interface` and `implementation` are contextual
  keywords, names elsewhere.
- **A unit is named by its path,** from the program's root, the directory
  of `main`'s file, as `lakes/depths.trendy` is `lakes.depths`, so
  nothing in the file names it. Delphi's `unit lakes;`, Haskell's
  `module Lakes where` and the like are removed, as is Delphi's `end.`;
  C++'s `public:` and `private:` become the headers; and Rust's `pub` and
  JavaScript's `export` are reported, with help (P0052). A unit has no
  section that runs as the program starts or ends, as Delphi's
  `initialization` does: a variable's initial value is computed when
  compiling, and the program starts in `main`.

### Using other units

A section names the units it uses with `uses`, and sees every name their
interfaces declare. A unit's name is its path from the program's root, the
directory of `main`'s file. A unit without `main`, checked on its own, is
part of some program above it, so its root is the nearest directory above it
where the units it uses are: `lakes/depths.trendy`, which uses `lakes.names`,
is `lakes.depths`, from the directory that has `lakes/names.trendy`. For the
unit `lakes/depths.trendy`:

```trendy,unit lakes.depths
interface

heading lake := { name: text, native: text, depth: integer };

operator deepest{lakes: relation lake}: relation lake;

implementation

operator deepest{lakes: relation lake}: relation lake [
    lakes where depth = max{r: lakes, of: depth}
];
```

a program uses it so:

```trendy
uses { lakes.depths as depths };

var lakes := relation lake {
    { name: "Baikal", native: "Байкал", depth: 1642 },
    { name: "Tanganyika", native: "Tanganyika", depth: 1470 },
};

operator main{} [
    call debug_table{r: deepest{lakes: lakes}};
];
```

- **It brings what the unit's interface declares:** its types, with their
  possible representations, its headings, its operators, its variables,
  its constants and its databases, each by its name, as the unit's own are. What the
  implementation keeps, the program doesn't see. A type, a heading or an
  operator the interface of a unit of the program declares, written where
  it isn't seen, gets a fix that names the unit in that section's `uses`,
  or, if several declare it, help naming them (A0003, A0015); a unit that
  uses this one, through any path, isn't suggested, as no unit uses
  itself.
- **Nothing passes on:** what a unit uses is its own, so the units that
  use it see none of it, and use what they need themselves.
- **The interface sees only its own `uses`,** as each unit that uses this
  one sees what the interface declares, which must say where what it
  mentions comes from; the implementation sees both sections' (A0067). A
  type or a heading the interface mentions from a unit only the
  implementation uses is fixed by using that unit in the interface
  instead.
- **A clash is where the name is written** (A0073): a name more than one
  of the other units it sees declares, the prelude among them, and the
  unit doesn't, means none of them, as nothing settles which by order. A
  unit that never writes it compiles, so a unit that gains a declaration
  breaks only the units that write its name, and none that declare it
  too. A name so written is qualified by the unit meant, as the help
  writes it. Operators of one name whose parameters' names differ are
  overloads, and don't clash.
- **A qualified name is the declaration of the unit it names:**
  `lakes.depths::metres` is the type or the heading `metres` that
  `lakes.depths`'s interface declares, whatever else the unit writing it
  sees of that name, `lakes.depths::deepest{lakes: l}` a call of its
  operator `deepest`, `call tally::record{…};` of an update operator's,
  and `seas.depths::metres{m: 281}` of a selector, and
  `trendy.prelude::count{r: r}` the prelude's `count`, though the unit
  writing it declares one of its own, which it hides there, and
  `lakes.depths::shallow_limit`, without braces, its constant, and
  `lakes.depths::known` its variable. Where a value goes, a qualified name
  is a variable, a constant or a call: its operator's name alone is an
  error, fixed to the call if it has no parameters (A0016), and its
  constant is never assigned (A0026). The qualifier is
  a unit the section uses, by the name its `uses` gives it after `as`, if
  it gives one, or else by its own, or the prelude, which every unit
  sees. A unit's own names are never qualified, as they hide every other
  unit's, so a qualifier of its own name is removed by the fix; one of a
  unit the section doesn't use gets a fix that uses it, and one of a unit
  it uses by another name, a fix that writes that (A0075). An unknown
  unit is fixed to the one it misspells (A0015), and so is a name it
  doesn't declare (A0003), whose error says if only its implementation
  does. Rust's `::` between a unit's parts is fixed to `.`, and, where a
  type goes, Java's and Python's `.` before the name to `::` (P0058).
- **A unit gives versions of the specifications it sees,** another
  unit's among them, for its own subtypes or another's, in its
  implementation: they are one family with the specification, and
  dispatch finds the same version from every unit. Over the whole
  program, each combination of subtypes has its version, and only one
  (A0063): as the units are in no order, each of several in different
  units is reported, naming the others. An operator of another unit's name and parameters' names
  that is no version of it is the unit's own, and clashes where a call
  names it.

- **First in its section, once** (A0071): the interface and the
  implementation each have their own, so that what their declarations see
  is said at their top. A fix moves one after a declaration before the
  first, and adds a second's units to the first's.
- **Each unit by its path** from the directory of `main`'s file, its
  directories and its file's stem between `.`s, as `lakes/depths.trendy`
  is `lakes.depths`, in braces, as the units can be in any order, and,
  after `as`, by one word the using unit calls it by instead, as `rename`
  gives an attribute another name, in the names it qualifies by the unit
  (see above).
- **Each a unit of the program** (A0015): main's, those its units use,
  and those that come with the compiler, `trendy.prelude`, which every
  unit sees without using it, `trendy.files`, which reads files, and
  `trendy.catalog`. A misspelling of one is fixed to it. The compiler is
  given main's file, and reads each unit it uses, through any path, from
  its file there; the names under `trendy.` are the compiler's own.
- **No unit uses itself, through any path** (A0072), so that a unit's
  interface is known before each unit that uses it is checked. What two
  units need of each other goes in a unit of its own, which each uses.
- **These rules are the catalog's constraints** (see [The
  catalog](#the-catalog)): a program that breaks one doesn't compile, and
  the tuples that break it are reported, each at the `uses` that says
  what they say.
- Other languages' imports are fixed to the `uses` they mean, as
  Python's `from lakes.depths import deepest` is to `uses { lakes.depths
  };`, and Delphi's `uses lakes.depths;` gets its braces (P0057). Where
  only the program knows what is meant, as in Rust's `use
  lakes::depths::deepest;`, the help names each reading.

```trendy,error
from lakes.depths import deepest

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
  in any order. `main` has none, since nothing passes it arguments. An
  update operator's parameter can be `var` or `out`, written before its
  name: the operator can then assign the variable it is passed (see [`var`
  and `out` parameters](#var-and-out-parameters)).
- **A body is a list of statements,** each ending with `;`: calls,
  assignments (see [Assignment](#assignment)), local variables'
  declarations (see [Local variables](#local-variables)), `if` statements
  (see [`if` statements](#if-statements)), loops (see [Loops](#loops)) and
  early exits (see [Early exits](#early-exits)). A
  read-only operator's ends with its value, an expression without `;`
  (see [Read-only operators](#read-only-operators)). An expression alone
  isn't a statement, as its value would go unused (P0035).
- **Arguments are a tuple of the parameters:** `call greet{name: "Ada",
  count: 2};`. Every argument is named, every parameter gets one, and the
  order doesn't matter. One for a `var` or `out` parameter is marked so
  too. A name alone is its own value, so `call greet{name, count};` is
  `call greet{name: name, count: count};`, and `call read{path, out lines};`
  marks what it needs to, as in a tuple (see
  [A name alone](#a-name-alone)).

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
- **`return x;` leaves the operator from anywhere in its body with the
  value `x`,** of its result type, as a loop's or an `if`'s block can (see
  [Early exits](#early-exits)). The body still ends with its value, which
  is written alone, so `return x;` at its end is fixed to `x` (P0031), and a
  `return` without a value is an error (A0035).
- **Its statements can decide what runs next:** an `if` statement, a loop,
  a transaction and a `fail` statement are what they are in an update
  operator, on its parameters, which it reads, and its local variables,
  which it reads and assigns, as RM Prescription 3 lets it update "ones
  that are purely local". They reach nothing else: they can't read or
  assign a variable of the program's (A0050, A0034), `call` an operator
  (A0034) or warn (A0035).
- **It can call itself,** directly or through others: see
  [Recursion](#recursion).
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
- **A call of one whose statements decide what runs next, or that calls
  itself, is always made when the program runs,** whatever its arguments, as
  they may not stop. So an initial value can't call one, directly or through
  another (A0077).
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

A read-only operator's statements run when it is called, on what it is
given:

```trendy
/* The powers of `base` up to `limit`. */
operator powers{base: integer, limit: integer}: relation { value: integer } [
    var result: relation { value: integer };
    var at := 1;
    while at <= limit loop [
        insert result relation { { value: at } };
        at := at * base;
    ];
    result
];

operator largest{numbers: array integer}: integer [
    var best := numbers[1];
    for n in numbers loop [
        if n > best then [
            best := n;
        ];
    ];
    best
];

operator main{} [
    call debug_table{r: powers{base: 3, limit: 30}};
    call debug_value{v: largest{numbers: array [3, 9, 5]}};
];
```

```trendy,error
operator doubling{n: integer}: integer [
    var at := 1;
    while at < n loop [
        at := at * 2;
    ];
    at
];

var size := doubling{n: 100};

operator main{} [];
```

### What a call can be given

A constraint in an operator's braces says which arguments a call of it can
have: `constraint condition`, with its parameters, a condition they satisfy.
Each call checks it first, before the operator runs. It says of a call what a
type's constraint says of a value: which of those its parameters' types allow
are the ones it takes (TTM RM Prescription 23).

```trendy
operator share{total: integer, people: integer, constraint people > 0}: integer [
    total div people
];

operator repeated{word: text, times_over: integer, constraint times_over >= 0, constraint word <> ""}: text [
    var result := "";
    var left := times_over;
    while left > 0 loop [
        result := result || word;
        left := left - 1;
    ];
    result
];

operator main{} [
    call debug_value{v: share{total: 1200, people: 3}};
    call debug_value{v: repeated{word: "na", times_over: 2}};
];
```

- **It reads the parameters it is about,** at least one (A0078), and whatever
  a read-only operator's value can: other read-only operators, a value block,
  and statements that decide what runs next. It reads no variable of the
  program's (A0050), and neither an `out` parameter, which has no value yet,
  nor a `per` parameter, which has one for each tuple (A0078). A `var`
  parameter's value is its variable's as the call starts. It can't call its
  operator, directly or through another (A0036).
- **A call that breaks it fails where the call is** (R0024), not in the
  operator, as the fix goes in the call. The error writes the call with the
  arguments the constraint reads: `share{people: 0} can't be called: it breaks
  the constraint `people > 0``. A transaction can take it, a
  `broken_call_constraint { called, constraint }`.
- **It is checked when compiling where what it reads is known,** whatever else
  isn't, so `share{total: t, people: 0}` is an error, with `t` a variable, and
  when the program runs where it isn't. That holds whichever statements the
  operator's body has, as the constraint is its own expression.
- **Callers see it.** An operator its unit's interface declares has its
  constraints there, and its definition doesn't repeat them (A0078, which
  fixes it). A specification's apply to every call, whichever version is
  computed, and a version has none of its own (A0078).
- **It is for what a call is given.** What a body computes is checked with an
  `if` and a `fail`, whose message says it in its own words (see
  [Failures](#failures)). A constraint written in a body is moved to the
  braces (P0055), and `assert` and `require`, as other languages write them,
  are errors that say which of the two to write (P0059). SQL's `CHECK` is
  fixed to `constraint`.
- **In braces, order means nothing,** so it can come before the parameters it
  reads, though the formatter keeps what is written, and `constraint` is a
  contextual keyword: a parameter can have the name, before a `:`.

```trendy,error
operator share{total: integer, people: integer, constraint people > 0}: integer [
    total div people
];

var each := share{total: 1200, people: 0};

operator main{} [];
```

```trendy,error
operator share{total: integer, people: integer}: integer [
    constraint people > 0;
    total div people
];

operator main{} [];
```

### Recursion

An operator can call itself, directly or through others, as RM Prescription 19
says a relational expression can, and Tutorial D's `TRANCLO` does. This one
makes values that its operand doesn't have, which is what `tclose` can't do:
each town's distance along each way there.

```trendy
heading leg := { start: text, stop: text, km: integer };

heading distance := { town: text, km: integer };

/* The distances `known`, and those along the legs `legs` from them. */
operator distances{legs: relation leg, known: relation distance}: relation distance [
    var onward := join{legs rename {km as more}, known rename {town as start}};
    var added := onward extend { total := km + more };
    var all := known union added{stop, total} rename {stop as town, total as km};
    if all = known then known else distances{legs, known: all}
];

operator main{} [
    var legs := relation {
        { start: "Xi'an", stop: "Dunhuang", km: 1700 },
        { start: "Dunhuang", stop: "Kashgar", km: 1900 },
        { start: "Dunhuang", stop: "Samarkand", km: 2700 },
        { start: "Kashgar", stop: "Samarkand", km: 900 },
    };
    call debug_table{r: distances{legs, known: relation { { town: "Xi'an", km: 0 } }}};
];
```

- **A tail call takes no stack and keeps no memory, however many follow.** This
  is a guarantee, not an optimization: no flag or build turns it off, and a
  program can count on it, so `distances` above needs no loop, and `known` lasts
  only as long as the call it is given to. A call that closes a cycle of calls is
  a tail call where it is in a tail position; no syntax says so. Before it runs,
  its caller gives up what it holds, its local variables and what its statement
  made, keeping only what the new arguments hold, and it returns to where its
  caller would have.
- **A tail position is** a read-only operator's value, a `return`'s value, but in
  a transaction, which commits after it, a branch of an `if` in a tail position,
  and the value of a value block in one; and an update operator's last statement,
  and the last statement of a branch of an `if` statement in one.
- **These hold for read-only and update operators, directly or through others:**
  in each of the operators below, whose calls would use up any stack otherwise.
- **A call with work left after it returns isn't a tail call:** an operand, as in
  `n + sum_to{n: n - 1}`; an argument; a local variable's value; `treat`'s
  operand, whose type is tested after; a call for each tuple of an `extend` or a
  `where`; a statement before the last; a call in a loop or a transaction; and a
  call whose `var` or `out` argument is a local variable of the caller's, which
  it would give up. A `var` or `out` parameter of the caller's passed on is fine,
  whose variable outlives the caller. A variable of the program's is never passed
  to a call that closes a cycle, as the operators would use it by its own name too
  (A0053).
- **Those that aren't can go as deep as the stack allows,** a gibibyte, millions
  of calls of a small operator. Going deeper is a failure like any other, a
  `too_deep` (R0025), with a message and exit status 1.
- **A failure's trace has no tail call's caller,** whose frame is gone, as a
  loop's has none of its earlier times around. It has the last of them, and the
  calls waiting: those it writes, if there are many, are the innermost and the
  outermost.
- **A call that reaches a cycle runs only when the program runs,** as a loop
  does, whatever its arguments, as it may not stop. So an initial value can't make
  one (A0077).
- **What nothing could stop is an error:** a type's constraint that selects its
  own type, a conversion that selects in the representation it converts, and a
  constraint in an operator's braces that calls the operator, each of which runs
  before anything could decide; a `fail` whose error's `message` leads back to it,
  which would go on writing errors (A0036); and calls that give an operator's type
  parameter ever larger types (A0041). A relation can't yet be defined from itself,
  as SQL's `WITH RECURSIVE` does.

```trendy
operator even{n: integer}: boolean [
    if n = 0 then true else odd{n: n - 1}
];

operator odd{n: integer}: boolean [
    if n = 0 then false else even{n: n - 1}
];

operator count_down{n: integer, var calls: integer} [
    calls := calls + 1;
    if n > 0 then [
        call count_down{n: n - 1, var calls};
    ];
];

operator main{} [
    var n := 100_000_000;
    call debug_value{v: even{n}};
    var calls := 0;
    call count_down{n, var calls};
    call debug_value{v: calls};
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
  requires, which the compiler checks of the prelude. `boolean`, `integer`,
  `approximate`, `text` and `character` are ordered; relations aren't, as their `<` is
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
*prelude*'s, trendy source the compiler includes, the unit
`trendy.prelude`, whose interface every unit sees without using it. A
few the compiler implements itself, the *built-ins*, which the prelude
declares with `builtin`, as it does the built-in types, and as the
compiler's other units declare theirs, such as `trendy.files` (see
[Reading and writing files](#reading-and-writing-files)); the rest its implementation defines in
trendy, as any unit's could, with its types' conversions and its
errors' messages, which no other unit sees. The prelude is checked with
every program, its
declarations as if the program had written them, so a program can extend
what it declares as it can its own, with a subtype of a union type or a
version of a specification (see [Union types](#union-types)), and an
operator is compiled into a program only if it calls it. A program can't
declare built-ins (A0013), but its units' names are their own: an
operator or a type of the name of one of the prelude's, built in or
written in trendy, hides the prelude's in its unit, which every other
unit still sees, as a unit's own name does a used unit's (see
[Units](#units)). Only the built-in types' names, `boolean`, `integer`,
`approximate`, `text` and `character`, are no unit's to declare (A0001).

The prelude keeps built-ins as few as possible: what can be written in
trendy is, so that it means exactly what its definition says, and when the
language can express more, built-ins become ordinary operators.

Its interface, what every unit sees of it:

```
/* The prelude, `trendy.prelude`: the types and operators of every
   program, which every unit sees, without using it, through its
   interface. Its built-ins, declared with `builtin`, the compiler
   implements; its other operators, and its types' conversions, its
   implementation gives in trendy. */
interface

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
builtin type character: { ordered };

builtin operator debug_value{v: any type};

/* Write `r` to stderr as a table: a column for each attribute, in name
   order, and a line for each tuple. */
builtin operator debug_table{r: any relation};

builtin operator count{r: any relation}: integer;

/* Whether `r` has no tuples, Tutorial D's `IS_EMPTY`. */
operator is_empty{r: any relation}: boolean;

/* The number of elements of `a`. */
builtin operator length{a: any array}: integer;

/* The integers from `from` to `to`, `step` apart, as PostgreSQL's
   `generate_series`: `from`, then `from + step`, and on as far as `to`,
   which is one of them only if a step lands on it. A negative step counts
   down. A `to` the other way from `from` gives no tuples. A step of 0 would
   get nowhere, so no call has one. */
operator series{
    from: integer,
    to: integer,
    step: integer,
    constraint step <> 0,
}: relation { value: integer };

/* The integers from `from` to `to`, one apart: none if `to` is less. */
operator series{from: integer, to: integer}: relation { value: integer };

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
operator some{r: any relation, of: boolean per r}: boolean;

/* Whether `of` is true for every tuple of `r`: whether no tuple makes it
   false. Of no tuples, it is true. */
operator every{r: any relation, of: boolean per r}: boolean;

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

/* A Unicode code point that a text can hold: from 0 to 10FFFF hex, but
   not the surrogates, D800 to DFFF, which UTF-8 can't encode, so each is a
   Unicode scalar value. It is a number, `value`, or the character of it alone,
   `character`, so that `code_point{character: '('}` is the code point of
   `(`, and `p.character` the character of `p`. Code points are ordered by their
   numbers. */
type code_point {
    { value: integer },
    {
        character: character,
        constraint length{a: to_code_points{from: from_character{from: character}}} = 1,
    },
    constraint value >= 0 and value <= 0x10FFFF and (value < 0xD800 or value > 0xDFFF),
    order [asc value],
};

/* A byte: a number from 0 to 255, written `x'7f'`, and ordered by its number,
   which is `b.value`. An array of them, `array byte`, is what binary data is,
   written `x"dead_beef"`. `and`, `or`, `xor` and `not` of bytes work on their
   bits. A byte isn't a character, or a number to do arithmetic on. */
type byte {
    { value: integer },
    constraint value >= 0 and value <= 0xFF,
    order [asc value],
};

/* The code points of `from`, in order: what its text is made of, one for
   each Unicode scalar value, not each character a reader sees, as `é` can
   be written as `e` and a combining accent, two code points. */
builtin operator to_code_points{from: text}: array code_point;

/* The text of the code points `from`, in order, which every array of them
   writes, so that it is `to_code_points`'s inverse. */
builtin operator from_code_points{from: array code_point}: text;

/* The characters of `from`, in order: what a reader sees as one each, by the rules of
   Unicode's UAX #29, as a flag is one of two code points. */
builtin operator to_characters{from: text}: array character;

/* The text of the characters `from`, in order, so that it is `to_characters`'s inverse
   of a text; but not of an array, as two characters can join, as a flag's halves do. */
builtin operator from_characters{from: array character}: text;

/* The text of the character `from`, alone. */
operator from_character{from: character}: text;

/* The character the text `from` is, which fails unless it is exactly one. */
operator parse_character{from: text}: character;

/* π, the ratio of a circle's circumference to its diameter: the
   approximate number nearest it. */
const pi: approximate;

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

/* What went wrong, when a program fails, or warns: a value of one of
   `error`'s subtypes, each a kind of failure, with what it is about as
   its components. `fail e` stops the program with `e`'s message, unless
   a transaction takes it, and `warn e` carries on. A program declares
   kinds of its own as the prelude does, each with its `message`. */
type error union;

/* What a failure says: a version for each of `error`'s subtypes. */
operator message{error: error}: text;

/* What a transaction collects, in the variable its `collecting` names:
   each error a statement in it failed with, which rolled it back, and
   each it warned with, which it carried on past. */
heading transaction_collection := { error: error, failed: boolean };

/* A failure a program describes in its own words, as in
   `fail failure{message: "the list is empty"}`. */
type failure is error { { message: text } };

/* A relation has `count` tuples where its only one is taken (R0005). */
type not_one_tuple is error { { count: integer } };

/* An array of `length` elements has none at `position` (R0011). */
type index_out_of_range is error { { position: integer, length: integer } };

/* `operation`, written as trendy writes it, as `7 div 0`, divides by zero
   (R0002). */
type division_by_zero is error { { operation: text } };

/* `operation`'s result, written as trendy writes it, as
   `9223372036854775807 + 1`, is out of its type's range (R0001). */
type overflow is error { { operation: text } };

/* Tuples being joined disagree on their common attribute `attribute`,
   whose values in them, `first` and `second`, written as `debug_value`
   writes them, differ, so they have no join (R0004). */
type tuple_mismatch is error { { attribute: text, first: text, second: text } };

/* A disjoint union, `d_union` or `d_insert`, adds a tuple, `value`,
   written as `debug_value` writes it, which is already there (R0006). */
type shared_tuple is error { { value: text } };

/* An included minus, `i_minus` or `i_delete`, removes a tuple, `value`,
   written as `debug_value` writes it, which isn't there (R0007). */
type missing_tuple is error { { value: text } };

/* The tuples `first` and `second`, written as `debug_value` writes them,
   of the relation about to be assigned to the variable `variable`, share
   their values for its key of the attributes `key`, separated by commas,
   which a key of none allows at most one tuple (R0008). */
type key_violated is error { { variable: text, key: text, first: text, second: text } };

/* As `key_violated`, of the relation-valued component `component` of a
   value selected of the type named `type` (R0008). */
type component_key_violated is error {
    { type: text, component: text, key: text, first: text, second: text },
};

/* Two changes in an `assign` block remove a tuple, `value`, written as
   `debug_value` writes it (R0009). */
type tuple_removed_twice is error { { value: text } };

/* Two changes in an `assign` block set the array element at `element`,
   its positions as trendy writes them, as `[2][1]`, or, if `within`, one
   sets it and another an element in it (R0009). */
type element_set_twice is error { { element: text, within: boolean } };

/* A relation has no tuples where `aggregate`, `min` or `max`, takes the
   least or greatest value of an attribute of them (R0010). */
type no_min_or_max is error { { aggregate: text } };

/* `text` isn't a value of the type named `type`, `integer`,
   `approximate` or `boolean`, that it is parsed as, or is one out of its
   range, if `out_of_range` (R0014). */
type unparsable is error { { text: text, type: text, out_of_range: boolean } };

/* `value`, written as `debug_value` writes it, selected of the type named
   `type`, breaks its constraint written `constraint` (R0017). */
type broken_constraint is error { { value: text, type: text, constraint: text } };

/* `of`, a negative number, has no square root (R0018). */
type no_square_root is error { { of: approximate } };

/* `value`, written as `debug_value` writes it, treated as one of the type
   named `type`, isn't one (R0019). */
type not_of_type is error { { value: text, type: text } };

/* `value`, written as `debug_value` writes it, is of both of the subtypes
   `first` and `second` of its type defined by constraints, but a value
   has one most specific type (R0020). */
type overlapping_subtypes is error { { value: text, first: text, second: text } };

/* `called`, a call written with the arguments the constraint written
   `constraint`, in the braces of the operator it calls, reads, breaks that
   constraint (R0024). */
type broken_call_constraint is error { { called: text, constraint: text } };

/* Calls, each with work left when it returns, have used up the `bytes` of
   the stack (R0025). */
type too_deep is error { { bytes: integer } };

/* A statement left the variables so that the constraint named
   `constraint` is false: its condition, or the operand of its `and` that
   is, written `condition` (R0023). */
type constraint_violated is error { { constraint: text, condition: text } };

/* As `constraint_violated`, with the tuples that make `condition` false,
   written as `debug_value` writes the relation of them: those of `a` that
   aren't in `b`, for `a <= b`, and those of `e`, for `is_empty{r: e}`
   (R0023). */
type constraint_violated_by is error { { constraint: text, condition: text, tuples: text } };
```

**`debug_value{v: any type}`** writes `v` to stderr, on one line, as trendy
source. Tuples and relations are written with their attributes in name
order, relations with their heading first and their tuples in a fixed order,
and an empty array with its elements' type, as in `array integer []`, so
the output reads back as the same value. A text's control characters, and
the invisible characters that reorder what follows them (Unicode's
bidirectional controls, such as U+202E), are written as escapes, as in
`"\u{202E}"`, so that what is written shows what the text holds, in its
order, and can't change the terminal it is written to. For example:
`relation { a: boolean, b: integer } { { a: false, b: 25 }, { a: true, b: 3 } }`.
The one exception is the attribute a `per` parameter's relation wraps its
tuples' own attributes in, which no source can name: it is written `…`,
as in `relation { of: text, … } { { of: "Nyeri", … } }`. It writes for the
programmer: what a program gives is written to standard output, with
`write` (see [Streams](#streams)).

**`debug_table{r: any relation}`** writes `r` to stderr as a table, for
reading rather than as source: a column for each attribute, in name order,
headed by its name, and a line for each tuple, in the same order as
`debug_value`'s. A cell is as wide as its column's widest, in a
terminal's columns, which a wide character, such as `李`, takes two of.
Numbers go to the right, and everything else to the left: a text without
its quotes, its control characters and bidirectional controls escaped as
in a literal, and a tuple,
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

**`series{from: integer, to: integer, step: integer}: relation { value:
integer }`** is the integers from `from` to `to`, `step` apart, as
PostgreSQL's `generate_series`, written in the prelude in trendy.
`series{from: integer, to: integer}` is the same one apart. A relation of
them is often what a program needs to compute with: below, a table of
squares, a count down, and some leap years.

```trendy
operator main{} [
    call debug_table{r: series{from: 1, to: 3} extend { square := value * value }};
    call debug_table{r: series{from: 9, to: 1, step: -3}};
    call debug_table{r: series{from: 1996, to: 2008, step: 4} rename {value as year}};
];
```

- **`to` is one of them only if a step lands on it,** so `series{from: 1,
  to: 10, step: 4}` is 1, 5 and 9. A negative step counts down.
- **A `to` the other way from `from` gives no tuples,** whatever the
  step's size: `series{from: 5, to: 1}` has none, as a count down needs
  its `step: -1`, and `series{from: 3, to: 3}` is 3.
- **A step of 0 would get nowhere,** so `series` has a constraint that
  says so, in its braces (see [What a call can be given](#what-a-call-can-be-given)): a call
  with one is an error when compiling, if the step is known then, whatever
  else isn't, and otherwise fails when the program runs (R0024).
- **Every integer can be an end,** as nothing is computed beyond `to`:
  `series{from: 9223372036854775806, to: 9223372036854775807}` is those
  two, with no overflow, and the largest steps reach across zero.
- **A relation has no order,** so it is a set of those values, not a
  sequence, where SQL's gives rows in order: a load puts them in an array
  in the order wanted (see [Loading a relation](#loading-a-relation)).
- **It runs when the program does,** as it has a loop (see
  [Read-only operators](#read-only-operators)), so an initial value can't
  call it (A0077).
- **Other languages' spellings:** `generate_series(1, 9, 2)` is fixed to
  `series{from: 1, to: 9, step: 2}`, as are `start`, `stop` and R's `by`
  for its parameters. Python's `range` leaves its end out, so it gets help
  instead (A0015).

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
so text too, by code point. Two values of a type of the program's own can tie in
an order of a representation other than the stored one, as rounding in a
conversion between approximate numbers can make them; of those, each gives
the first in canonical order, whether computed when compiling or running. Of no tuples, neither has a value, so either
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
**`parse_approximate{from: text}: approximate`**,
**`parse_boolean{from: text}: boolean`**,
**`parse_character{from: text}: character`** and
**`from_character{from: character}: text`** convert values, which
never convert implicitly: *The Third Manifesto* allows no coercion, only
explicit conversions such as Tutorial D's `CAST_AS_CHAR` (chapter 3).

- **`to_text` writes any value as trendy source,** the text `debug_value`
  writes, so a value of every type has one, and it reads back as the same
  value: `42`, `5e-1` for an approximate number, which is always written
  with an exponent, `"Lima"` for a text, with its quotes, which a
  text never needs, being text already, and `'a'` for a character, with
  its, whose text alone is `from_character{from: c}`.
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

### Reading and writing files

Reading a file, and writing one, is the work of `trendy.files`, a unit that
comes with the compiler, which a unit uses as it does any other, `uses {
trendy.files };`. Its interface declares the operators that read and write
files, which the compiler implements, as the prelude's built-ins are, and the
errors only a file gives, each a subtype of the prelude's `error`; its
implementation gives their messages. It is a unit of every program, so
its names are as any unit's: a unit can declare a `malformed_csv` of its
own, and then the language's is `trendy.files::malformed_csv`, and its own
is called by its unit's name too if another unit uses `trendy.files` (see
[Units](#units)). Calling one of its
operators, or naming one of its errors, in a unit that doesn't use it is
an error whose fix uses it (A0015, A0003). Its interface, what a unit
that uses it sees:

```
/* Reading and writing files: the lines of a text file, the bytes of any
   file, and the records of a CSV file, each read or written when the
   program runs, so by an update operator, with the errors only a file
   gives. A unit that reads or writes one uses it, `uses { trendy.files };`. */
interface

/* The path of a file, where it is: relative to the directory the program
   runs in, as `file_path{text: "rivers.txt"}`, unless it is absolute, as
   `file_path{text: "/tmp/rivers.txt"}`. Its text is the path as the system
   takes it, so it isn't empty, which names no file, and has no U+0000,
   which no system's path can hold. */
type file_path { { text: text }, constraint is_path_text{text} };

/* Whether `text` can be a file's path: it isn't empty, and has no U+0000. */
operator is_path_text{text: text}: boolean;

/* Read the lines of the text file at `path` into `lines`, in the file's
   order, so that its first line is the array's element 1. A line ends at a line feed, or a carriage return and a line feed,
   which aren't part of it; the last needs neither. Each call reads the
   file as it is when the program runs: the compiler never opens it, so
   it needn't exist until then, and two calls can read different lines,
   so it is an update operator, which `call` invokes. A file that can't
   be read fails (R0012), as does one that isn't UTF-8 text (R0013). */
builtin operator load_text_file{path: file_path, out lines: array text};

/* Read the lines of the text file at `path` into `lines`, as the other
   `load_text_file` does, but rather than stopping the program for a file
   that can't be read, or isn't UTF-8 text, give no lines, and its error,
   an `unreadable_file` or a `not_utf8`, in `errors`, which is empty when
   the file is read. */
builtin operator load_text_file{
    path: file_path,
    out lines: array text,
    out errors: relation { error: error },
};

/* Write `lines` to the text file at `path`, in order, each ending in a
   line feed, so that `load_text_file` reads the same lines back. A file is
   made if there is none, in a directory that must exist, and what
   one holds is replaced. Each call writes the file when the program runs,
   so it is an update operator, which `call` invokes. A file that can't be
   written fails (R0026), and can be left with part of `lines` if that
   happens partway, as on a full disk. A line with a line feed in it, or
   that ends in a carriage return, wouldn't read back as one line, and fails
   before anything is written (R0027). */
builtin operator save_text_file{path: file_path, lines: array text};

/* Write `lines` to the text file at `path`, as the other `save_text_file`
   does, but rather than stopping the program for a file that can't be
   written, give its error, an `unwritable_file`, in `errors`, which is empty
   when the file is written. A line that wouldn't read back as one still
   stops the program, as that is a mistake in the call and not the file's. */
builtin operator save_text_file{
    path: file_path,
    lines: array text,
    out errors: relation { error: error },
};

/* Read the bytes of the file at `path` into `bytes`, whatever they are, in
   the file's order, so that its first byte is the array's element 1. Each
   call reads the file as it is when the program runs, as
   `load_text_file` does, so it is an update operator, which `call`
   invokes. A file that can't be read fails (R0012). */
builtin operator load_binary_file{path: file_path, out bytes: array byte};

/* Read the bytes of the file at `path` into `bytes`, as the other
   `load_binary_file` does, but rather than stopping the program for a file
   that can't be read, give no bytes, and its error, an `unreadable_file`,
   in `errors`, which is empty when the file is read. */
builtin operator load_binary_file{
    path: file_path,
    out bytes: array byte,
    out errors: relation { error: error },
};

/* Read the records of the CSV file at `path` into `rows`, in the file's
   order, each a tuple whose attributes are the fields in the columns
   `columns` gives them: it has the tuples' attributes, each the position
   of its column, counting from 1, as `tuple { city: 1, people: 3 }`
   reads `city` from the first column and `people` from the third. A
   file's columns have an order and no names, so the call names them.
   Columns it doesn't name are skipped. With `header`, the file's first
   record is a header, which is skipped too. Each attribute is text, an
   integer, an approximate number, a boolean or a character, read from its field as
   `parse_integer`, `parse_approximate`, `parse_boolean` and `parse_character` read
   them, and text as it is, or a value of a type of the program's own whose
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
builtin operator load_csv{path: file_path, header: boolean, columns: C, out rows: array T}
    types { C: any tuple, T: any tuple };

/* Read the records of the CSV file at `path` into `rows`, as the other
   `load_csv` does, but rather than stopping the program for a file it
   can't read, give no rows, and its error in `errors`, and for a record it
   can't, skip it, giving its error there: an `unreadable_file`, a
   `not_utf8`, a `malformed_csv`, a `missing_column` or an
   `unreadable_field`, or, for a value that breaks its type's constraints,
   or its components' keys, or is of two of its subtypes, a
   `broken_constraint_in_file`, a `component_key_violated_in_file` or an
   `overlapping_subtypes_in_file`. A quoted field that isn't closed ends
   the records read. `errors` is empty when every record is read. */
builtin operator load_csv{
    path: file_path,
    header: boolean,
    columns: C,
    out rows: array T,
    out errors: relation { error: error },
}
    types { C: any tuple, T: any tuple };

/* The file at `path` can't be read, for `reason`, the system's words for
   why, as "No such file or directory" (R0012). */
type unreadable_file is error { { path: text, reason: text } };

/* The file at `path` can't be written, for `reason`, the system's words
   for why, as "Permission denied" (R0026). */
type unwritable_file is error { { path: text, reason: text } };

/* The element `line`, counting from 1, of the lines to write to the file at
   `path` has a line feed in it, or ends in a carriage return, so it wouldn't
   read back as one line (R0027). */
type not_one_line is error { { path: text, line: integer } };

/* The file at `path` isn't UTF-8 text, from the line `line` on (R0013). */
type not_utf8 is error { { path: text, line: integer } };

/* The CSV file at `path` isn't CSV on the line `line`, as `problem` says
   (R0015). */
type malformed_csv is error { { path: text, line: integer, problem: text } };

/* `attribute`, or a component, as `columns` names it, is read from the
   column `column`, counting from 1, but the CSV file at `path` has
   `columns` columns (R0016). */
type missing_column is error { { path: text, attribute: text, column: integer, columns: integer } };

/* `first` and `second`, attributes, or components, as `columns` names
   them, are both read from the column `column`, but each needs a column
   of its own (R0016). */
type column_shared is error { { first: text, second: text, column: integer } };

/* `attribute`, or a component, as `columns` names it, is read from the
   column `column`, below 1, but columns count from 1 (R0016). */
type column_below_one is error { { attribute: text, column: integer } };

/* The field `text`, in the column `column`, counting from 1, on the line
   `line` of the CSV file at `path`, isn't a value of `type`, the type of
   `attribute`, or a component, as `columns` names it, or is one out of its
   range if `out_of_range` (R0014). */
type unreadable_field is error {
    {
        path: text,
        line: integer,
        column: integer,
        attribute: text,
        type: text,
        text: text,
        out_of_range: boolean,
    },
};

/* The record on the line `line` of the CSV file at `path` gives
   `attribute`, or a component, as `columns` names it, `value`, written as
   `debug_value` writes it, which isn't a value of `type`: it breaks the
   constraint `constraint` (R0017). */
type broken_constraint_in_file is error {
    { path: text, line: integer, attribute: text, value: text, type: text, constraint: text },
};

/* The record on the line `line` of the CSV file at `path` gives
   `attribute`, or a component, as `columns` names it, a value of `type`
   whose component `component`, a relation, has two tuples, `first` and
   `second`, written as `debug_value` writes them, that share their values
   for its key of the attributes `key`, separated by commas, which a key
   of none allows at most one tuple (R0008). */
type component_key_violated_in_file is error {
    {
        path: text,
        line: integer,
        attribute: text,
        type: text,
        component: text,
        key: text,
        first: text,
        second: text,
    },
};

/* The record on the line `line` of the CSV file at `path` gives
   `attribute`, or a component, as `columns` names it, `value`, written as
   `debug_value` writes it, which is of both of the subtypes `first` and
   `second` of its type defined by constraints, but a value has one most
   specific type (R0020). */
type overlapping_subtypes_in_file is error {
    { path: text, line: integer, attribute: text, value: text, first: text, second: text },
};
```

**A path is a `file_path`,** a value of `trendy.files`' own type, not a text,
as a date is a `date` and not the text of one: `file_path{text: "rivers.txt"}`.
Its text is the path the system takes, relative to the directory the program
runs in unless it is absolute, so it isn't empty, which names no file, and has
no U+0000, which no system's path can hold; a text that breaks either fails to
be selected (R0017). A text where a path goes is an error (A0004) whose fix
selects one of it, as it does for any type with a possible representation of
one component of the value's type, `uuid{text: …}` among them.

```trendy
uses { trendy.files };

const rivers := file_path{text: "rivers.txt"};

operator main{} [
    var lines: array text;
    call load_text_file{path: rivers, out lines};
    call debug_value{v: rivers.text};
];
```

```trendy,error
uses { trendy.files };

operator main{} [
    var lines: array text;
    call load_text_file{path: "rivers.txt", out lines};
];
```

**`load_text_file{path: file_path, out lines: array text}`** reads input: the
lines of a text file, into the variable its call gives `lines`.

```trendy
uses { trendy.files };

operator main{} [
    var lines: array text;
    call load_text_file{path: file_path{text: "rivers.txt"}, out lines: lines};
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
- **Unless the call names `out errors`,** a `relation { error: error }`:
  then such a file gives no lines, and its error in `errors`, which is
  empty when the file is read. The error is an `unreadable_file{path,
  reason}` or a `not_utf8{path, line}`, values of `trendy.files`'s
  subtypes of `error` (see [Failures](#failures)), which the program can
  test and read, write the message of, or fail with.

```trendy
uses { trendy.files };

operator count_lines{path: file_path} [
    var lines: array text;
    var errors: relation { error: error };
    call load_text_file{path: path, out lines: lines, out errors: errors};
    if is_empty{r: errors} then [
        call debug_value{v: length{a: lines}};
    ] else [
        call debug_value{v: message{error: value from errors}};
    ];
];

operator main{} [
    call count_lines{path: file_path{text: "rivers.txt"}};
];
```

```trendy,error
uses { trendy.files };

operator main{} [
    var lines := load_text_file{path: file_path{text: "rivers.txt"}};
];
```

```trendy,error
uses { trendy.files };

var lines := load_text_file{path: file_path{text: "rivers.txt"}};

operator main{} [];
```

**`save_text_file{path: file_path, lines: array text}`** writes output: the
lines of an array to a text file, each ending in a line feed.

```trendy
uses { trendy.files };

operator main{} [
    var rivers: array text;
    call load_text_file{path: file_path{text: "rivers.txt"}, out lines: rivers};
    call save_text_file{path: file_path{text: "copy.txt"}, lines: rivers};
];
```

- **It is an update operator,** which `call` invokes, as anything that changes
  what is outside the program is (TTM RM Prescription 3), and a transaction's
  statements call it at no depth (A0066): nothing undoes a write.
- **What it writes reads back as it was.** `load_text_file` gives the same lines,
  however few, so a line with a line feed in it, or that ends in a carriage
  return, which wouldn't read back as one line, stops the program before anything
  is written (R0027). `out errors` doesn't take that: it is a mistake in the call.
- **A file is made if there is none, and what one holds is replaced.** The
  directory it is in must exist. A relative path is from the directory the program
  runs in. `write`, of `trendy.streams`, adds to what a file holds instead (see
  [Streams](#streams)).
- **A file that can't be written stops the program** (R0026), unless the call
  names `out errors`, which is then empty, or holds its `unwritable_file { path,
  reason }`. A failure partway, as on a full disk, can leave part of the lines in
  the file, and what it held before is gone.

**`load_binary_file{path: file_path, out bytes: array byte}`** reads input: the
bytes of any file, into the variable its call gives `bytes`, an `array byte`
(see [Bytes](#bytes)).

```trendy
uses { trendy.files };

operator main{} [
    var bytes: array byte;
    call load_binary_file{path: file_path{text: "kente.png"}, out bytes};
    call debug_value{v: length{a: bytes}};
    call debug_value{v: bytes[1] = x'89'};
];
```

- **The bytes are an array, in the file's order,** whatever they are: nothing
  reads them as text, so a file needn't be UTF-8, and a line's end is two bytes,
  or one, as any others are. The first byte is element 1, and an empty file has
  none.
- **It is an update operator,** as `load_text_file` is, and reads the file when
  the program runs, relative to the directory it runs in, stopping it if the
  file can't be read (R0012). .NET's and Java's `ReadAllBytes`, Python's
  `read_bytes`, Kotlin's `readBytes` and Ruby's `binread` are fixed to it, then
  to a call with the variable its `out` argument.
- **Unless the call names `out errors`,** as `load_text_file`'s can: then a file
  that can't be read gives no bytes, and its `unreadable_file` in `errors`, which
  is empty when the file is read.

**`load_csv{path: file_path, header: boolean, columns: C, out rows: array T}`**,
where `C` and `T` are any tuples, reads the records of a CSV file into the
variable its call gives `rows`, each a tuple of the columns `columns` gives
its attributes.

```trendy
uses { trendy.files };

heading city := { name: text, people: integer, km2: approximate, capital: boolean };

operator main{} [
    var rows: array tuple city;
    call load_csv{
        path: file_path{text: "cities.csv"},
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
  positions are known then, and otherwise stops the program, with a
  `column_shared { first, second, column }` or a `column_below_one {
  attribute, column }`, mistakes in the call, which `out errors`
  doesn't take.
- **`header` says whether the first record is a header,** which is then
  skipped, as RFC 4180's `header=present` says. With `header: false`,
  the first record is a row, and if one of its fields isn't its
  attribute's type, the error says the line may be a header.
- **The array's type says what to read.** Each field is read as its
  attribute's type, as Tutorial D's `LOAD` takes its shape from its
  target (chapter 5): text as it is, and an integer, an approximate
  number, a boolean or a character as `parse_integer`, `parse_approximate`,
  `parse_boolean` and `parse_character` read them, so `2672.3` is an approximate
  number, and `yes` and `N` are booleans. A field that isn't one stops the program
  (R0014), writing it, its column, its line, and its attribute, an
  `unreadable_field`. An attribute of another
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
- **Unless the call names `out errors`,** a `relation { error: error }`,
  as `load_text_file`'s can: then a file that can't be read gives no rows
  and its error, an `unreadable_file`, a `not_utf8` or a `missing_column`
  (R0016), and each record that can't be is skipped, with its error, a
  `malformed_csv` (R0015) or an `unreadable_field` (R0014), as is one
  whose value breaks its type's constraints, with a
  `broken_constraint_in_file` (R0017), or its components' keys, with a
  `component_key_violated_in_file` (R0008), or is of two of its subtypes
  defined by constraints, with an `overlapping_subtypes_in_file` (R0020),
  and the rest are read. A quoted
  field that isn't closed ends the records read. Two attributes of one
  column, or one of a column below 1, are the call's mistakes, not the
  file's, so they still stop the program.

```trendy
uses { trendy.files };

operator load_cities{path: file_path} [
    var rows: array tuple { city: text, people: integer };
    var errors: relation { error: error };
    call load_csv{
        path: path,
        header: true,
        columns: tuple { city: 1, people: 2 },
        out rows: rows,
        out errors: errors,
    };
    call debug_value{v: length{a: rows}};
    call debug_value{v: (errors extend { message := message{error: error} }){message}};
];

operator main{} [
    call load_cities{path: file_path{text: "cities.csv"}};
];
```

```trendy,error
uses { trendy.files };

operator main{} [
    var rows: array tuple { city: text, people: integer };
    call load_csv{
        path: file_path{text: "cities.csv"},
        header: true,
        columns: tuple { city: 1, people: 1 },
        out rows: rows,
    };
];
```

### Streams

A stream is where a program reads or writes a piece at a time, as against the
whole of a file, which `trendy.files` loads and saves. A program is started
with three, its standard input, output and error, which the system connects
to a terminal, a pipe or a file, and a file at a path is one too. Writing to
one is the work of `trendy.streams`, a unit that comes with the compiler,
which a unit uses as it does `trendy.files`, `uses { trendy.streams };`. Its
interface:

```
/* Streams: where a program reads and writes a piece at a time, as against
   the whole of a file, which `trendy.files` loads and saves. A program is
   started with three, its standard input, output and error, which the
   system connects to a terminal, a pipe or a file, and a file at a path is
   one too. A stream is outside the program: a value of `stream` names one,
   and update operators, which `call` invokes, read from it and write to
   it. A unit that does uses it, `uses { trendy.streams };`. */
interface

uses { trendy.files };

/* Where a program reads or writes, a piece at a time. */
type stream union;

/* One of the three streams a program is started with, by its name. */
type standard_stream is stream {
    { name: text },
    constraint name = "input" or name = "output" or name = "error",
};

/* The file at `path`. */
type file is stream { { path: file_path } };

/* What the program is given to read. */
const standard_input := standard_stream{name: "input"};

/* Where the program writes what it gives, and where `write` writes unless a
   call says otherwise. */
const standard_output := standard_stream{name: "output"};

/* Where the program writes what it says of how it runs, as it writes its
   failures. */
const standard_error := standard_stream{name: "error"};

/* Write `text` to `to`, as it is: what is written next goes on from where
   it ends, on its line, as an answer to a prompt does. What a call writes
   to standard output or error follows what earlier ones wrote to either, in
   the order they ran. Writing to standard output can fail, as when a disk is
   full (R0030), and when what reads it has stopped, as `head` does after its
   lines, the program ends there. A file is written at its end, after what it
   holds, and made if there is none, in a directory that must exist: each
   call opens it and closes it, and a file that can't be written fails
   (R0026). */
operator write{text: text, to: stream, constraint to <> standard_input};

/* Write `text` to standard output, as the other `write` does. */
operator write{text: text};

/* Write `line` to `to`, then a line feed, which ends it. */
operator write{line: text, to: stream, constraint to <> standard_input};

/* Write `line` to standard output, then a line feed. */
operator write{line: text};

/* Write each of `lines` to `to`, in order, each followed by a line feed: to
   a file, all at once. */
operator write{lines: array text, to: stream, constraint to <> standard_input};

/* Write each of `lines` to standard output, each followed by a line feed. */
operator write{lines: array text};

/* Write `bytes` to `to`, as they are. */
operator write{bytes: array byte, to: stream, constraint to <> standard_input};

/* Write `bytes` to standard output, as they are. */
operator write{bytes: array byte};

/* Read the next line of standard input into `line`, without the line feed,
   or the carriage return and line feed, that ends it, as `load_text_file`
   reads a file's lines: the last needs neither. What standard output holds
   is written first, so that a prompt written with `write{text}` shows
   before the program waits for its answer. Each call reads on from where
   the last stopped, so it is an update operator. At the end of standard
   input there is no line, which fails (R0031), as does a line that isn't
   UTF-8 text (R0013), or standard input that can't be read (R0032). */
builtin operator read{out line: text};

/* Read the next line of standard input into `line`, as the other `read` of
   a line does, but rather than stopping the program where it has none, give
   no line, and its error, an `end_of_input`, a `not_utf8_input` or an
   `unreadable_input`, in `errors`, which is empty when a line is read. */
builtin operator read{out line: text, out errors: relation { error: error }};

/* Read the rest of standard input, to its end, into `lines`, in order, each
   as `read{out line}` reads one: none, if it has ended already. Input that
   isn't UTF-8 text fails (R0013), and so does standard input that can't be
   read (R0032). */
builtin operator read{out lines: array text};

/* Read the rest of standard input into `lines`, as the other `read` of lines
   does, but rather than stopping the program for input it can't, give no
   lines, and its error, a `not_utf8_input` or an `unreadable_input`, in
   `errors`, which is empty when the lines are read. */
builtin operator read{out lines: array text, out errors: relation { error: error }};

/* Read the rest of standard input, to its end, into `bytes`, whatever they
   are, in order: none, if it has ended already. Standard input that can't be
   read fails (R0032). */
builtin operator read{out bytes: array byte};

/* Read the rest of standard input into `bytes`, as the other `read` of bytes
   does, but rather than stopping the program for input it can't, give no
   bytes, and its error, an `unreadable_input`, in `errors`, which is empty
   when the bytes are read. */
builtin operator read{out bytes: array byte, out errors: relation { error: error }};

/* Standard output can't be written, for `reason`, the system's words for
   why, as "No space left on device" (R0030). */
type unwritable_output is error { { reason: text } };

/* Standard input has ended, after `lines` lines, so there is no line to read
   (R0031). */
type end_of_input is error { { lines: integer } };

/* Standard input isn't UTF-8 text on its line `line`, counting from 1
   (R0013). */
type not_utf8_input is error { { line: integer } };

/* Standard input can't be read, for `reason`, the system's words for why, as
   "Bad file descriptor" (R0032). */
type unreadable_input is error { { reason: text } };
```

**`write`** writes text, a line, lines or bytes, each its parameter's name, to
the stream `to` names, and to standard output if a call names none.

```trendy
uses { trendy.streams, trendy.files };

operator main{} [
    call write{text: "Habari, "};
    call write{line: "dunia!"};
    call write{lines: array ["Nile", "Paraná"]};
    call write{line: "wrote three lines", to: standard_error};
    call write{line: "greeted", to: file{path: file_path{text: "greetings.log"}}};
];
```

- **The program never holds a stream.** A stream is outside it, as a file's
  bytes are, and a value of `stream` names one, as a path names a file:
  `standard_input`, `standard_output` and `standard_error`, constants of
  `standard_stream`, or `file{path: p}`. Nothing in the program points to
  one, as a handle would, since what a pointer points to is a variable (*The
  Third Manifesto*, OO Proscription 2), and nothing opens or closes one: a
  write to a file opens it and closes it. Tutorial D has no I/O at all
  (chapter 5).
- **What is written is in the order it was written,** whichever of standard
  output and standard error each call wrote to, so a terminal or a file that
  both go to shows them as the program wrote them, `debug_value`'s and the
  program's failures among them. Standard output holds what it is given, and
  writes it all at once, when it is full, before anything is written to
  standard error, and as the program ends, or after each write, on a
  terminal, where someone reads it as the program runs: the program never
  says when.
- **It is an update operator,** which `call` invokes, as anything that
  changes what is outside the program is (TTM RM Prescription 3), and a
  transaction's statements call it at no depth (A0066): nothing undoes a
  write. `debug_value` and `debug_table` write for the programmer, to
  standard error, and are exempt.
- **`text` is written as it is,** so what is written next goes on from where
  it ends, as an answer to a prompt does; **`line`** ends it with a line feed,
  and **`lines`** writes each so. Text is written as UTF-8, and **`bytes`**, an
  `array byte`, as they are.
- **A file is written at its end,** after what it holds, and made if there is
  none, in a directory that must exist, so `write` adds to what
  `save_text_file` replaces. A file that can't be written stops the program
  (R0026). Each call opens the file and closes it, so `lines` writes them all
  at once.
- **Standard input can't be written:** a call that names it breaks the
  constraint `to <> standard_input` (R0024), an error when compiling.
- **Standard output can fail,** as when a disk fills, which stops the program
  (R0030): the write that finds it out can be a later one than the one whose
  bytes the system didn't take, as it holds them first. A pipe whose reader
  has stopped, as `head` does once it has its lines, ends the program there,
  quietly, as the system ends it.
- **Other languages' names are fixed to it:** Python's `print`, Java's and
  Rust's `println`, Ruby's and C's `puts`, Pascal's `writeln` and the shell's
  `echo` to `write{line: …}`, and Rust's `eprintln` to `write{line: …, to:
  standard_error}`, each with its unit used. C's, Ruby's and Python's
  `stdin`, `stdout` and `stderr` are fixed to `standard_input`,
  `standard_output` and `standard_error`.

```trendy,error
operator main{} [
    call println{"Habari"};
];
```

```trendy,error
uses { trendy.streams };

operator main{} [
    call write{line: "Habari", to: standard_input};
];
```

**`read`** reads standard input: its next line, `read{out line}`, or the rest of
it, to its end, as lines, `read{out lines}`, or as bytes, `read{out bytes}`.

```trendy
uses { trendy.streams };

operator main{} [
    call write{text: "Name? "};
    var name: text;
    call read{out line: name};
    call write{line: "Habari, " || name || "!"};
    var rest: array text;
    call read{out lines: rest};
    call write{line: to_text{from: length{a: rest}} || " more lines"};
];
```

- **Each call reads on from where the last stopped,** so two reads give two lines,
  and it is an update operator, which a transaction's statements call at no depth
  (A0066). The program holds no position in standard input: the system does.
- **A prompt shows before the program waits.** What standard output holds is
  written before standard input is read, through a pipe too, so `write{text: "Name?
  "}` shows before the answer is typed.
- **A line ends as a file's does,** at a line feed, or a carriage return and a line
  feed, which aren't part of it, and the last needs neither, as `load_text_file`
  reads them. A line is UTF-8 text, or the program stops (R0013); `read{out bytes}`
  reads whatever bytes it is given.
- **At the end there is no line,** and `read{out line}` stops the program, saying
  how many lines were read (R0031). `read{out lines}` and `read{out bytes}` give
  what is left, none at the end.
- **Unless the call names `out errors`:** then the end, a line that isn't UTF-8
  text, or standard input that can't be read (R0032), gives no line, and its error
  there, an `end_of_input { lines }`, a `not_utf8_input { line }` or an
  `unreadable_input { reason }`.
- **It reads standard input only, for now.** A file is read whole by `trendy.files`,
  as a file has no next line unless something holds a position in it.
- **Other languages' names are explained:** Python's `input`, Ruby's and C's
  `gets`, Kotlin's `readLine`, Rust's `read_line`, C's `getline` and Java's
  `nextLine` are reported with what to write instead (A0015).

```trendy
uses { trendy.streams };

/* Each name on standard input, greeted, until it ends. */
operator main{} [
    while true loop [
        var name: text;
        var errors: relation { error: error };
        call read{out line: name, out errors};
        if not is_empty{r: errors} then [
            leave;
        ];
        call write{line: "Habari, " || name || "!"};
    ];
];
```

```trendy,error
operator main{} [
    var name := input{"Name? "};
];
```

### UUIDs

`trendy.uuid`, a unit that comes with the compiler, which a unit uses with
`uses { trendy.uuid };`, has RFC 9562's universally unique identifiers. It is
trendy, so nothing in it is built in: a type with two possible representations,
16 bytes and the text RFC 9562 writes, and a subtype of it for each version a
program can tell by its bits. Its interface, what a unit that uses it sees:

```
/* UUIDs, RFC 9562's universally unique identifiers, and the versions of them
   whose fields a program can tell: 16 bytes, written as text in 36 characters.
   A unit that has them uses this one, `uses { trendy.uuid };`. */
interface

uses { trendy.time };

/* A UUID: its 16 bytes, or its text, as RFC 9562 writes it, lowercase, each byte
   two hexadecimal digits, with dashes after the 4th, 6th, 8th and 10th, as
   `"00112233-4455-6677-8899-aabbccddeeff"`. Ordered by its bytes. */
type uuid {
    { bytes: array byte, constraint length{a: bytes} = 16 },
    { text: text, constraint is_uuid_text{text} },
    order [asc bytes],
};

/* Whether `text` is a UUID's text, which `uuid{text}` can select. */
operator is_uuid_text{text: text}: boolean;

/* A version 4 UUID: its 7th byte's high bits say version 4, and its 9th's say
   RFC 9562's variant. Every UUID whose bytes do is one. */
type uuid_v4 is uuid { constraint (bytes[7] and x'f0') = x'40' and (bytes[9] and x'c0') = x'80' };

/* A version 7 UUID: its 7th byte's high bits say version 7, and its 9th's say
   RFC 9562's variant. Every UUID whose bytes do is one. */
type uuid_v7 is uuid { constraint (bytes[7] and x'f0') = x'70' and (bytes[9] and x'c0') = x'80' };

/* Give `v4` a new version 4 UUID: 122 random bits. A transaction's statements can
   call it, as they are computed from what was read once, before anything else. Its
   argument can be a `uuid` variable, as any `out` parameter's can be of a supertype. */
operator generate{out v4: uuid_v4};

/* Give `v7` a new version 7 UUID, which sorts by when it was made: the time by the
   system's clock, to the millisecond, and 74 random bits. It reads the clock, so a
   transaction's statements give it the time, as the next says (A0066). */
operator generate{out v7: uuid_v7};

/* Give `v7` a new version 7 UUID for the instant `at`, on or after 1970-01-01 and
   before the year 10889, as its 48 bits of milliseconds hold no other. In a
   transaction, `at` is what `transaction_start` gives. */
operator generate{
    at: timestamp,
    out v7: uuid_v7,
    constraint at.date.days >= 0 and at.date.days < 3_257_812,
};
```

```trendy
uses { trendy.uuid };

const id := uuid{text: "6ba7b810-9dad-41d1-80b4-00c04fd430c8"};

operator main{} [
    call debug_value{v: id};
    call debug_value{v: id.text};
    call debug_value{v: id.bytes[7]};
    call debug_value{v: id is uuid_v4};
    call debug_value{v: is_uuid_text{text: "6BA7B810"}};
    var made: uuid_v7;
    call generate{out v7: made};
    call debug_value{v: (made.bytes[7] and x'f0') = x'70'};
];
```

- **A value of a version's type is a UUID whose bits say so.** Each subtype is
  defined by a constraint, so `uuid{bytes: …}` of a version 4 UUID's bytes is a
  `uuid_v4`, and `id is uuid_v4` is true of it (see [Subtypes defined by
  constraints](#subtypes-defined-by-constraints)).
- **Its text is lowercase,** as RFC 9562 writes it, so that each UUID has one text,
  as a possible representation must: `uuid{text: …}` of uppercase digits, or
  anything else `is_uuid_text` says isn't one, fails (R0017).
- **Which version, the call says:** `var id: uuid; call generate{out v4: id};`, as an
  `out` argument can be of a supertype of its parameter's type. `generate{out uuid}`,
  which gave the nil UUID before trendy had random numbers, is fixed to `out v4`
  (A0010).
- **`generate` makes new UUIDs.** It is an update operator, as what gives a new value
  each call isn't a function of its arguments (RM Prescription 3). Its bits are computed,
  by ChaCha20 from what the system gives once, as the program first asks for any, so a
  transaction's statements can call it: nothing reads the world again, and a rollback
  doesn't take the bits back, so nothing undone is given again.
- **A version 7 UUID also has the time,** to the millisecond, so that it sorts by when it
  was made. `generate{out v7}` reads the clock, so a transaction's statements can't call
  it (A0066), and `generate{at, out v7}` takes the instant, which in a transaction is
  what `transaction_start` gives. Its 48 bits reach from 1970 to the year 10889, and any
  other instant fails where the call is (R0024).

### Dates and times

`trendy.time`, a unit that comes with the compiler, which a unit uses with
`uses { trendy.time };`, has `date`, `time`, `timestamp`, `offset_timestamp` and
`duration`. It is trendy, but for reading the clock. Its interface, what a unit that uses it sees:

```
/* Dates and times: a day of the calendar, a time of day, an instant, an instant with
   the UTC offset it was written at, and how long something takes. A unit that has them
   uses this one, `uses { trendy.time };`. */
interface

/* A date: how many days it is after 1970-01-01, negative before it, or its year,
   month and day, on the Gregorian calendar, whose rules reach every year, with 1 BC
   as year 0. It is within 10,000,000,000,000 days of 1970-01-01, about 27 billion
   years either way, and ordered by its days. */
type date {
    { days: integer, constraint days >= -10_000_000_000_000 and days <= 10_000_000_000_000 },
    {
        year: integer,
        month: integer,
        day: integer,
        constraint year >= -30_000_000_000 and year <= 30_000_000_000,
        constraint month >= 1 and month <= 12,
        constraint day >= 1 and day <= days_in_month{year, month},
    },
    order [asc days],
};

/* How many days `month` of `year` has, 28 to 31. */
operator days_in_month{
    year: integer,
    month: integer,
    constraint month >= 1 and month <= 12,
}: integer;

/* A time of day: how many nanoseconds it is after midnight, or its hour, minute,
   second and nanosecond. Every day has 86,400 seconds: there are no leap seconds,
   so a `second` of 60 isn't one. Ordered by its nanoseconds. */
type time {
    { nanos_of_day: integer, constraint nanos_of_day >= 0 and nanos_of_day < 86_400_000_000_000 },
    {
        hour: integer,
        minute: integer,
        second: integer,
        nanosecond: integer,
        constraint hour >= 0 and hour < 24,
        constraint minute >= 0 and minute < 60,
        constraint second >= 0 and second < 60,
        constraint nanosecond >= 0 and nanosecond < 1_000_000_000,
    },
    order [asc nanos_of_day],
};

/* An instant: a date and a time, in UTC. Every date and every time make one, so
   selecting it can't fail. Ordered by its date, then its time. */
type timestamp {
    { date: date, time: time },
    order [asc date, asc time],
};

/* An instant with the offset from UTC it was written at, in seconds, ahead of UTC if
   positive and less than a day either way: a `timestamp`, or the year, month, day,
   hour, minute, second and nanosecond the clock there showed. The same instant at
   two offsets is two values, so to compare instants, compare their `timestamp`s.
   Ordered by its `timestamp`, then its offset. */
type offset_timestamp {
    {
        timestamp: timestamp,
        utc_offset: integer,
        constraint utc_offset > -86_400 and utc_offset < 86_400,
    },
    {
        year: integer,
        month: integer,
        day: integer,
        hour: integer,
        minute: integer,
        second: integer,
        nanosecond: integer,
        utc_offset: integer,
        constraint year >= -30_000_000_000 and year <= 30_000_000_000,
        constraint month >= 1 and month <= 12,
        constraint day >= 1 and day <= days_in_month{year, month},
        constraint hour >= 0 and hour < 24,
        constraint minute >= 0 and minute < 60,
        constraint second >= 0 and second < 60,
        constraint nanosecond >= 0 and nanosecond < 1_000_000_000,
    },
    order [asc timestamp, asc utc_offset],
};

/* How long something takes, exactly: its whole seconds, rounded down, and the
   nanoseconds past them, so half a second back is `seconds: -1`, `subsec_nanos:
   500_000_000`. Every day has 86,400 seconds, so a day and 24 hours are one value. A
   month has no fixed length, so it isn't a duration: see `add_months`. It is within
   2,000,000,000,000,000,000 seconds, about 63 billion years, either way, which holds
   the time between any two `timestamp`s, and ordered by how long it is. */
type duration {
    {
        seconds: integer,
        subsec_nanos: integer,
        constraint seconds >= -2_000_000_000_000_000_000 and seconds <= 2_000_000_000_000_000_000,
        constraint subsec_nanos >= 0 and subsec_nanos < 1_000_000_000,
    },
    order [asc seconds, asc subsec_nanos],
};

/* `n` of a unit as a duration, negative for that long back. */
operator nanoseconds{n: integer}: duration;

operator microseconds{n: integer}: duration;

operator milliseconds{n: integer}: duration;

operator seconds{
    n: integer,
    constraint n >= -2_000_000_000_000_000_000 and n <= 2_000_000_000_000_000_000,
}: duration;

operator minutes{
    n: integer,
    constraint n >= -33_333_333_333_333_333 and n <= 33_333_333_333_333_333,
}: duration;

operator hours{
    n: integer,
    constraint n >= -555_555_555_555_555 and n <= 555_555_555_555_555,
}: duration;

operator days{
    n: integer,
    constraint n >= -23_148_148_148_148 and n <= 23_148_148_148_148,
}: duration;

/* How long after `from` it is `to`, negative if `to` comes first. Every two instants
   have one, so this can't fail. */
operator elapsed{from: timestamp, to: timestamp}: duration;

/* The instant `by` after `t`. It fails past the end of a `date`'s range. */
operator after{t: timestamp, by: duration}: timestamp;

/* The instant `by` before `t`. It fails past the end of a `date`'s range. */
operator before{t: timestamp, by: duration}: timestamp;

/* Whether the month `months` after `d`'s has `d`'s day, within a `date`'s range, so
   that `add_months` can be called. */
operator has_day_after_months{d: date, months: integer}: boolean;

/* The same day of the month `months` after `d`'s, or before it if negative. A month
   that hasn't that day, as February hasn't a 31st, is a broken constraint: nothing
   says which other day was meant. */
operator add_months{d: date, months: integer, constraint has_day_after_months{d, months}}: date;

/* Give `at` the instant it is, by the system's clock, which can be set back, so a
   later read can give an earlier instant. It reads the world, so a transaction's
   statements can't call it, at any depth (A0066): they have `transaction_start`. */
operator now{out at: timestamp};

/* Give `at` the instant the outermost transaction running began: the same however
   often it is read, in whatever is nested in it, and a new one if it is run again.
   Every call of it runs in a transaction, so one that can run in none is an error
   (A0082). It is when the transaction began, not when what it does takes effect. */
operator transaction_start{out at: timestamp};
```

```trendy
uses { trendy.time };

const new_year := date{year: 2024, month: 3, day: 20};
const equinox := timestamp{
    date: new_year,
    time: time{hour: 3, minute: 6, second: 26, nanosecond: 0},
};

operator main{} [
    call debug_value{v: new_year.days};
    call debug_value{v: date{days: new_year.days + 30}.month};
    call debug_value{v: equinox.time.hour};
    var kathmandu := offset_timestamp{timestamp: equinox, utc_offset: 20_700};
    call debug_value{v: kathmandu.hour};
    call debug_value{v: kathmandu.minute};
    var lima := offset_timestamp{timestamp: equinox, utc_offset: -18_000};
    call debug_value{v: lima.day};
    call debug_value{v: lima = kathmandu};
    call debug_value{v: lima.timestamp = kathmandu.timestamp};
];
```

- **A `timestamp` is a `date` and a `time`,** with no constraint of its own, so
  `timestamp{date: d, time: t}` can't fail, whatever the date and the time. `t.date`
  and `t.time` read them, without computing anything, and it is in UTC.
- **An `offset_timestamp` is a `timestamp` and where it was written:** its other
  representation is what the clock there showed, `kathmandu.hour`, which can be
  another date, and `offset_timestamp{year: …, utc_offset: …}` selects one from that.
  `utc_offset` is the same component in both (see [Possible
  representations](#a-programs-own-types)). The offset is part of the value, as it
  can be read (RM Prescriptions 5 and 8), so the same instant at two offsets is two
  values, unequal, and they don't join: compare their `timestamp`s for the instant.
- **Every value can be read, at the ends too:** a `date` is within
  10,000,000,000,000 days of 1970-01-01, about 27 billion years, far enough from
  overflowing that every conversion works, so a clock past the last date, which an
  offset can show, reads as the year it is, and selects the value again.
- **No leap seconds:** every day has 86,400 seconds, so `second: 60` fails (R0017).
- **`days_in_month{year, month}`** is the rule a `date` is checked by, so a program can
  check a date before selecting one, which otherwise stops it.
- **No `+` between them yet.** Compute with a component and select the result,
  `date{days: d.days + 30}`, as the error on `d + 30` says (A0022), and use the
  operators below on instants, which `t - s`, `t + d` and `t - d` are fixed to (A0022).
- **Text isn't a date,** but `"2024-03-05"` where a `date` is expected is fixed to
  `date{year: 2024, month: 3, day: 5}`, and so are ISO 8601's forms of the others
  (A0004), a `duration`'s `"PT1H30M"` among them. `datetime` and `instant` are fixed to
  `timestamp`, `datetimeoffset` to `offset_timestamp`, and `interval` and `timedelta`
  to `duration` (A0003).
- **Written in the stored representation,** as every value is, for now:
  `date{days: 19802}`.

```trendy
uses { trendy.time };

/* A flight from Lagos to Nairobi, as each city's clock shows it. */
const leaves := offset_timestamp{
    year: 2024,
    month: 3,
    day: 20,
    hour: 9,
    minute: 0,
    second: 0,
    nanosecond: 0,
    utc_offset: 3_600,
};
const lands := offset_timestamp{
    year: 2024,
    month: 3,
    day: 20,
    hour: 15,
    minute: 25,
    second: 0,
    nanosecond: 0,
    utc_offset: 10_800,
};

var bookings: relation { reference: text, at: timestamp } key { reference };

operator book{reference: text} [
    var at: timestamp;
    call transaction_start{out at};
    insert bookings relation { { reference, at } };
];

operator main{} [
    var checked: timestamp;
    call now{out at: checked};
    call debug_value{v: checked.date.year >= 2024};

    transaction [
        call book{reference: "NBO-1"};
        call book{reference: "NBO-2"};
    ];
    call debug_value{v: count{r: bookings{at}}};

    const flight := elapsed{from: leaves.timestamp, to: lands.timestamp};
    call debug_value{v: flight = minutes{n: 265}};
    call debug_value{v: after{t: leaves.timestamp, by: flight} = lands.timestamp};
    call debug_value{v: flight < hours{n: 5}};

    const rent_paid := date{year: 2024, month: 1, day: 31};
    call debug_value{v: add_months{d: rent_paid, months: 2}.month};
    call debug_value{v: has_day_after_months{d: rent_paid, months: 1}};
];
```

- **A `duration` is an exact length of time,** the one form of each: `hours{n: 24}` and
  `days{n: 1}` are one value, as every day has 86,400 seconds, and two durations are
  equal, or one less than the other, as long as they are. Its stored representation
  rounds its seconds down, so half a second back is `seconds: -1`, `subsec_nanos:
  500_000_000`: `.seconds` of a negative one isn't its whole seconds towards zero.
- **`elapsed{from, to}` can't fail,** as the time between any two instants is one. A
  single count of nanoseconds in an `integer` would reach only about 292 years, and a
  `timestamp` reaches 27 billion, which is why it has two components.
- **`after{t, by}` and `before{t, by}`** move an instant by a duration, either way, and
  fail past the end of a `date`'s range (R0017).
- **`now{out at}` reads the system's clock.** It is an update operator, as what the
  clock shows is no function of anything (RM Prescription 3), so a read-only operator
  is given what it reads as an argument. The clock can be set back, so a later read can
  give an earlier instant. It reads the world, so a transaction's statements can't call
  it, at any depth (A0066).
- **`transaction_start{out at}` gives the instant the outermost transaction running
  began:** the same however often it is read, and in whatever is nested in it. A
  transaction has it from before any of its statements run, so reading it reads nothing,
  and a transaction run again has a new one. It is when the transaction began, not when
  what it does takes effect. Every call of it runs in a transaction, so a call in `main`
  that isn't in one, and that leads to it through calls that aren't either, is an error
  (A0082), whose fix for a call of it itself is `now`. An operator that begins a
  transaction of its own can be called from anywhere.
- **A month has no fixed length, so it isn't a `duration`,** as it would be equal to
  some number of seconds, and adding the two would differ (RM Prescription 8).
  `months{n: 1}` is an error that says so (A0015).
- **`add_months{d, months}`** gives the same day of another month, and a month that
  hasn't that day breaks its constraint, when compiling if the arguments are known
  (R0024): nothing says which other day was meant. `has_day_after_months{d, months}`
  says first whether it can be called. So January 31st plus two months is March 31st,
  and plus one is an error.

```trendy,error
uses { trendy.time };

var due := add_months{d: date{year: 2024, month: 1, day: 31}, months: 1};

operator main{} [];
```

### Arguments and the environment

`trendy.process`, a unit that comes with the compiler, which a unit uses with
`uses { trendy.process };`, has what the program was started with: its
arguments and its environment, each a variable the runtime gives its value as
the program starts, before `main` runs, a `builtin var`. Its interface, what a
unit that uses it sees:

```
/* `trendy.process`: what the program was started with, its arguments and
   its environment, which the runtime gives it as it starts, before `main`
   runs, if anything it runs reads or assigns them, so that what a program
   never looks at can't stop it. */

interface

/* A variable of the environment: its name and its value. */
heading variable := { name: text, value: text };

/* The arguments the program was started with, in the order given, each as
   it was written, but the program's path. One that isn't UTF-8 text stops
   the program before `main` runs (R0028). */
builtin var arguments: array text;

/* The environment the program was started with: each of its variables, the
   first of each name, as the system's `getenv` finds them, so none without
   a name. One that isn't UTF-8 text, its name or its value, stops the
   program before `main` runs (R0029). */
builtin var environment: relation variable key { name };

/* Whether a process can be given the variable `name`, of `value`, which the
   system writes `name=value`, ended by NUL: its name isn't empty, and has no
   `=`, and neither has NUL. */
operator can_be_given{name: text, value: text}: boolean;

/* Every variable of the environment is one a process can be given. */
constraint given := is_empty{r: environment where not can_be_given{name, value}};

/* An argument the program was started with, at `position`, from 1, isn't
   UTF-8 text, so `arguments` can't have it: the program stops before `main`
   runs (R0028). */
type not_utf8_argument is error { { position: integer } };

/* A variable of the environment the program was started with isn't UTF-8
   text, its name or its value, so `environment` can't have it: the program
   stops before `main` runs (R0029). `name` is its name, each byte of it
   that isn't UTF-8 written as U+FFFD. */
type not_utf8_environment is error { { name: text } };
```

```trendy
uses { trendy.process };

operator main{} [
    for each in arguments loop [
        call debug_value{v: each};
    ];
    var home := environment where name = "HOME";
    if not is_empty{r: home} then [
        call debug_value{v: value from home{value}};
    ];
];
```

- **They are variables, which any unit that uses the unit reads and
  assigns,** as it does any other unit's (see [Units](#units)). An initial
  value can't read them, as only the program knows them (A0077), nor can a
  read-only operator, which is given what it needs of them (A0050).
  `length{a: arguments}` is how many arguments there are, and `environment
  where name = "HOME"` the variable `HOME`, if there is one.
- **The program is given them only if something it runs reads or assigns
  them,** so that what it never looks at can't stop it: a program that reads
  its arguments alone is never stopped by its environment.
- **An argument, or a variable of the environment, that isn't UTF-8 text
  stops the program before `main` runs** (R0028, R0029), saying which, as it
  can't be text, and dropping it or changing its bytes would have the program
  act on what it wasn't given. An argument is said by its position, from 1, and
  a variable by its name, each byte of it that isn't UTF-8 written as U+FFFD.
- **The environment is what the system's `getenv` finds:** the first variable
  of each name, as a name is its key, and none without a name, nor an entry
  without `=`. Its constraint, which a variable a program adds keeps too, is
  what a process can be given: a name, without `=`, and neither with NUL. It
  is the program's own, which nothing outside it changes while it runs, nor
  does assigning it change anything outside it.
- **Other languages' names are fixed:** `main{arguments: …}`, `main{args:
  …}` and Java's `String[] args` are removed from `main`, which uses
  `trendy.process` instead (A0019), and `argv`, `argc`, `environ` and `env`
  elsewhere say where the arguments and the environment are (A0015).

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
read-only one (RM Prescription 3), and gone once it returns. One written
`const` rather than `var` is a constant, which never changes: see
[Constants](#constants).

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
  declaration runs, from the parameters and variables then (a local
  constant's when the compiler chooses: see [Constants](#constants)). A type
  parameter has no default, since each call gives it a type, nor has a
  tuple with one, and a relation or an array with one starts empty, as any
  does. A local variable of a type with no default can be declared without a
  value, for a statement to give it one, an assignment or an `out` argument, on
  every path to where it is read, which is otherwise an error (A0054). A read-only
  body that nothing in decides what runs next could as well give it its value where
  it is declared, and has to (A0006).
  A relation's keys are checked whenever it is assigned (R0008), and when
  compiling if the value is known then.
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

### Constants

`const name[: type] := value;` is a name for a value that never changes, at the top
level of a unit or anywhere a local variable can be declared: `var` and `const` are
Rust's `let mut` and `let`. It is read as a variable is, by any operator, and by a
type's constraint, where a variable can't be (A0050), as nothing about it can change.
TTM asks for the same of relations, "relation constants or relcons" (chapter 6, under
RM Prescription 14), as `TABLE_DEE` and `TABLE_DUM` are.

```trendy
const moves := relation {
    { direction: "^", dx: 0, dy: 1 },
    { direction: "v", dx: 0, dy: -1 },
    { direction: ">", dx: 1, dy: 0 },
    { direction: "<", dx: -1, dy: 0 },
}
key { direction };

const digits := series{from: 0, to: 9} rename {value as d};
const largest := max{r: digits, of: d};

type digit { { n: integer }, constraint n >= 0 and n <= largest };

operator dx{toward: text}: integer [
    value from (moves where direction = toward){dx}
];

operator share{total: integer, people: integer}: integer [
    const each := total div people;
    const left := total - each * people;
    each + left
];

operator main{} [
    call debug_value{v: dx{toward: ">"}};
    call debug_value{v: digit{n: largest}};
    const guests := 3;
    var served := 0;
    while served < guests loop [
        const portion := share{total: 10, people: guests};
        call debug_value{v: portion};
        served := served + 1;
    ];
];
```

- **A constant of the program's reads no variable** (A0050), but anything else a
  value can: literals, other constants, in any order, and read-only operators. So
  its value is the same whenever it is computed. One that reads itself, directly or
  through the operators it calls, is an error (A0021).
- **A read is as if the constant's expression were written there,** in parentheses,
  as Tutorial D's `WITH` defines a name (TTM, chapter 5), and how much of it to
  compute, and when, is chosen as the program runs, since only that says how often each
  read runs. One the compiler can compute is as if its value were written there.
  Otherwise, as `digits`' is above, with the loop in `series`, nothing is computed
  until a read needs it, and nothing if none does:
  - **A read that narrows it**, as a restriction of a closure to one value, which needs
    only that value's paths, or of a join by what one of its operands has, computes only
    as much of it as it needs the first time it runs. From its next run on, as in a
    loop, it computes the constant whole, at most once, and the reads after it share
    that. A read that would need all of it anyway, as a restriction by an attribute an
    `extend` adds to it does, is a read of all of it.
  - **A read of all of it** computes it whole, at most once, and shares it too.
  - **A read that is its only one, and runs at most once,** always computes as far as
    it needs.

  A variable's initial value, known when compiling, can't read one of those (A0077).
- **A constant's key is of all of it.** What reads it, the compiler too, takes the key
  to hold, so the constant is computed whole before any read, and a key broken anywhere
  fails every read, though the tuples a read narrows it to are fine.
- **A failure in computing what a read needs stops the program as any other does,**
  when that first happens, after what the program has done by then: where the constant
  is declared. Narrowing never leaves out what can fail, so computing the whole fails
  just where a read of part of it would, but for running out of memory, which the whole
  can where a part doesn't. As a constant has one value, whenever it is computed, a read
  of all of it counts as unable to fail, so the compiler can compute it before the order
  things are written in says, as it can move any operand that can't fail (see
  [Evaluation](#evaluation)).
- **A local constant has the value its expression has where it is declared,** from
  the parameters and variables then, each time that runs: in a loop, each time round.
  But when it is computed is the compiler's to choose, as for the program's, if nothing
  it reads changes before its reads. If it has one read, which runs once each time the
  declaration does, it is as if its expression were written there, so that `const t :=
  r join s;` read as `t join u` is one join of three, in whatever order costs least. If
  it has more, a read that narrows it computes only what it needs the first time it runs
  after the declaration does, and from its next run on the constant is computed whole,
  once, and shared, as a constant of the program's is. So a
  failure in computing it can come after what runs before its read, though it says it is
  where the constant is declared, and one read nowhere fails nowhere. A local variable,
  `var`, is computed where it is declared, and so is a constant with a key, which is of
  all of it. A `const` parameter is the same thing, the value its caller gives.
- **It has its value where it is declared** (A0080), as nothing could give it one
  later: `const limit: integer;` is an error, and a variable is what starts at its
  type's default. But an interface can leave it to the implementation, as an
  operator's body is, with its type: `const pi: approximate;` there, and `const pi
  := …;` here, so that other units see nothing of how it is computed.
- **It is never assigned** (A0026): not by `:=`, `insert`, `delete`, `update` or
  `load`, nor passed to a `var` or `out` parameter. The fix declares it `var`.
- **Otherwise it is declared as a variable is,** with a type, or its value's, and
  keys, checked as it is computed, in the namespace the program's variables and
  databases share. It isn't called: `pi{}` is fixed to `pi`, and `moves{a}` is a
  projection of `moves`, as it is of a variable. Another unit reads it by its name,
  or by its name qualified by the unit's, `trendy.prelude::pi`.

```trendy,error
operator main{} [
    const count := 0;
    count := count + 1;
];
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
- **In an operator's body, read-only too, and in a value block,** on the
  local variables and parameters each can assign (see
  [Read-only operators](#read-only-operators) and
  [Value blocks](#value-blocks)).
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

- **A loop ends once its block makes its condition false,** or leaves it
  (see [Early exits](#early-exits)). A condition written `true`, or known
  when compiling to be true, never ends, and is an error, unless the block
  can leave, with `leave` or `return`; and so is one written `false`, and an
  empty block (A0046).
- **Its block is a block in brackets,** even of one statement, as an `if`
  statement's branches are (P0029), with the same fixes for other
  languages' blocks (P0003). Each run's local variables are its own,
  declared afresh, and out of scope after the loop, in its condition too.
- **In an operator's body, read-only too, and in a value block,** on the
  local variables and parameters each can assign (see
  [Read-only operators](#read-only-operators) and
  [Value blocks](#value-blocks)).
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

### Early exits

`return` leaves the operator it is in, `leave;` leaves the innermost loop
it is in, and `iterate;` goes on to that loop's next run, past the rest of
its block, from wherever each is, inside `if` statements, transactions and
other loops too. Tutorial D's `RETURN` and `LEAVE` are the first two
(chapter 5), and its "variant of `<leave>` that merely terminates the current
iteration of the loop and begins the next" the third.

```trendy
operator first_negative{a: array integer}: integer [
    for n in a loop [
        if n < 0 then [
            return n;
        ];
    ];
    0
];

operator odd_sum{a: array integer}: integer [
    var total := 0;
    for n in a loop [
        if n mod 2 = 0 then [
            iterate;
        ];
        total := total + n;
    ];
    total
];

operator greet{names: array text} [
    var i := 0;
    while true loop [
        i := i + 1;
        if i > length{a: names} then [
            leave;
        ];
        call debug_value{v: "karibu, " || names[i]};
    ];
];

/* The first row with a negative number in it, but for rows with a zero,
   which `iterate rows;` goes on from, past the rest of the inner loop. */
operator first_negative_row{grid: array array integer}: integer [
    var row := 0;
    for cells in grid loop as rows [
        row := row + 1;
        for cell in cells loop [
            if cell = 0 then [
                iterate rows;
            ];
            if cell < 0 then [
                return row;
            ];
        ];
    ];
    0
];

operator main{} [
    call debug_value{v: first_negative{a: array [3, -4, -1]}};
    call debug_value{v: odd_sum{a: array [1, 2, 3, 4, 5]}};
    call greet{names: array ["Amara", "Kofi"]};
    call debug_value{v: first_negative_row{grid: array [array [0, -1], array [2, -3]]}};
];
```

- **A read-only operator's `return` gives its value,** `return x;`, and an
  update operator's gives none, `return;` (A0035), once each of its `out`
  parameters is assigned, on every path to there (A0054). A value block
  can't return, as it would leave the block, not the operator (A0035).
  `return f{…};` of an operator that calls itself, directly or through
  others, is a tail call, as one at the body's end is (see
  [Recursion](#recursion)).
- **Each goes past the end of what it is in,** as that would end: a block's
  local variables are gone, and a transaction it leaves commits, as at its
  block's end, collecting its warnings (see [Transactions](#transactions)).
  So `leave` or `return` from a transaction keeps what its statements did.
- **`leave` and `iterate` are of the innermost loop,** or, with a name, of
  the loop around them named so, `loop as name`, past the end of the loops
  within it, and of one of the body, or the value block, they are in: a
  value block's statements compute its value, and a loop around it is no loop
  of theirs, so one in no loop, or of a name no loop around it has, is an
  error, fixed to a name it misspells (A0084). `iterate` checks a loop's
  condition first, as after each run, so it can end a `loop [ … ] while c;`
  too. After `leave` or `iterate` in a `loop` that runs once, nothing its
  block assigns after them is assigned for sure.
- **A loop's name follows its `loop`,** `while c loop as scan [ … ];`, `loop
  as retry [ … ] while c;` and `for x in a loop as each [ … ];`, as `as`
  names what is before it elsewhere. Only a `leave` or an `iterate` in the
  loop names it, so it is no name of anything else's, and clashes with no
  variable or attribute; a loop in another can't have its name, as one of
  them couldn't name it then (A0001), though loops side by side can share
  one. A name nothing names changes nothing, which the fix removes (A0084),
  and one written before the loop, `scan: while …`, as Tutorial D, Java and
  JavaScript write it, is fixed to `loop as scan` (P0032).
- **Nothing after an exit in its block runs,** nor after a `fail`, or an
  `if` whose branches all leave theirs, so a statement there is an error,
  which the fix deletes (A0085). An exit that skips nothing is one too,
  which the fix deletes (A0084): `iterate;` at the end of its loop's block,
  and `return;` at the end of an update operator's body, which the block
  comes to anyway, in an `if` there too.
- **`leave` and `iterate` are contextual keywords,** names elsewhere, and
  other languages' `break` and `continue` are fixed to them (P0032).

```trendy,error
operator first_over{a: array integer, limit: integer} [
    for n in a loop [
        if n > limit then [
            call debug_value{v: n};
            break;
        ];
    ];
];

operator main{} [];
```

```trendy,error
operator sign{n: integer}: integer [
    if n < 0 then [
        return -1;
    ] else [
        return 1;
    ];
    var zero := n = 0;
    0
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
  component is reported as an argument is. No other operator of its unit
  can have a type's name (A0057); another unit's of its name, the
  prelude's among them, is hidden by it there, or overloaded.
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
  than another: a variable of the program's of one, or of a tuple with one,
  needs an initial value (A0006), and a local variable a statement gives one
  before anything reads it (A0054).
- **A value is written as its selector's invocation,** by `debug_value`,
  `debug_table` and `to_text`: `position{latitude: -1.29e0, longitude:
  3.682e1}`, which reads back as the same value. It is in the stored
  representation, the one the others convert to, whose components are
  the value itself, so the text needs no conversion to read back.
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
  failure says which one a value breaks. A relation-valued component's
  keys, written after its type, are checked before them (see
  [Keys](#keys)).
- **A constraint is a condition on the components alone,** in scope by
  name. It reads no variable of the program's, whose value changes
  (A0050), and selects no value of its own type, directly or through
  operators, which would check it again without end (A0036). One known
  when compiling, true or false whatever the components, is an error
  (A0058).
- **The braces are a set,** so the constraints and the possible
  representation can come in any order. SQL's `check` is fixed to
  `constraint` (P0040). A constraint inside the components' braces,
  where Tutorial D writes a possible representation's `CONSTRAINT`, is
  that representation's own (see below).

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
- **A type has one order** (A0008). It goes in the type's braces, since
  it compares values, not the components given to one representation's
  selector, and is moved there from the components' (P0041).

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
  says which, unless both conversions carry it unchanged, `c := c`, which makes it
  the stored one's, the same component in both. Each conversion computes each
  component it should, once, of its type (A0009, A0008, A0004), and reads no
  variable (A0050), so a value is the same in any representation.
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
  `replace` or an `update`, as in a constraint: `body replace { kelvin
  := celsius }` is fine.
- **In an `assign` block, `update`s of one representation's components
  combine,** as a tuple's attributes' do, into one selection in it, and
  `update`s of two representations' conflict (R0009).
- **`load_csv` reads a value in any representation,** from the columns
  `columns` gives its components, as in `mean: tuple { celsius: 3 }`,
  and selects it in that one (see [The prelude](#the-prelude)).

A possible representation can have constraints of its own, after its
components in its braces, as Tutorial D writes a possrep's `CONSTRAINT`
(*The Third Manifesto*, RM Prescription 23): they say which of its
components represent a value, where its `to` would make one of others
too:

```trendy
type duration {
    { seconds: integer },
    { hours: integer, minutes: integer, constraint minutes >= 0 and minutes < 60 }
        to { seconds := hours * 3600 + minutes * 60 }
        from { hours := seconds div 3600, minutes := seconds mod 3600 div 60 },
    constraint seconds >= 0,
};

operator main{} [
    var flight := duration{hours: 7, minutes: 45};
    call debug_value{v: flight};
    call debug_value{v: flight.minutes};
];
```

- **They are checked as a value is selected in the representation,** by
  its selector, `replace` or `update` in it, or `load_csv` reading it, of
  the components given, before its `to` converts them, as TTM checks a
  possrep's constraint in its selector: `duration{hours: 1, minutes:
  90}` fails (R0017), though `to` would make a value of it, two and a
  half hours, which `duration{hours: 2, minutes: 30}` is. The failure
  names the components as the selector was given them.
- **They name the representation's own components alone** (A0074),
  which are all a selection in it has; the type's constraints, in the
  type's braces, are checked of every value, in any representation,
  through its conversions, so they name any representation's. A fix
  moves a constraint that names another's into the type's braces.
- **One that selects a value in its representation,** directly or
  through operators, would check itself again (A0036). Selecting one in
  another representation is fine.
- **The stored representation's own hold of every value,** as the type's
  do, since its components are every value's, in whatever representation
  it is selected: `{ seconds: integer, constraint seconds >= 0 }` would
  say what `constraint seconds >= 0` in the type's braces says. They name
  its own components alone all the same, as the others' do, so that
  nothing in them tells the type's users which representation is stored
  (see [Units](#units)).

```trendy,error
type duration {
    { seconds: integer },
    { hours: integer, minutes: integer, constraint minutes >= 0 and minutes < 60 }
        to { seconds := hours * 3600 + minutes * 60 }
        from { hours := seconds div 3600, minutes := seconds mod 3600 div 60 },
    constraint seconds >= 0,
};

var long := duration{hours: 1, minutes: 90};

operator main{} [];
```

### Union types

A union type has no values of its own: each of its values is a value of
one of its subtypes, each a type declared `is` it (*Type Inheritance and
Relational Theory*, chapter 12, IM Prescription 20). Every plane figure
is an ellipse or a polygon:

```trendy
/* Every plane figure is an ellipse or a polygon. */
type plane_figure union;

type ellipse is plane_figure {
    { a: approximate, b: approximate },
    constraint a >= b,
};
type polygon is plane_figure { { corners: integer } };

var lake := ellipse{a: 3e0, b: 2e0};
var field := polygon{corners: 4};

/* A value of either kind is a plane figure. */
var figures := relation {
    { name: "Victoria", figure: lake },
    { name: "Zócalo", figure: field },
};

operator main{} [
    var figure: plane_figure := lake;
    call debug_value{v: lake.a};
    call debug_value{v: figure = lake};
    call debug_table{r: figures};
];
```

- **A union type has no possible representation of its own,** and so
  no selector and no components, which makes it what Date calls a dummy
  type. Its values are selected by its subtypes' selectors:
  `plane_figure{a: 3e0, b: 2e0}` is fixed to the selector of the
  subtype whose components those are, `ellipse{a: 3e0, b: 2e0}` (A0061).
  A union type with a representation of its own, all of whose values
  are of its subtypes, isn't in trendy yet (A0060); a type with one has
  subtypes defined by constraints instead (see
  [Subtypes defined by constraints](#subtypes-defined-by-constraints)).
- **A subtype names its union type after `is`,** between its name and
  its braces, and is otherwise a type of the program's own like any
  other, with its representations, constraints and order. Java's
  `extends`, and Kotlin's and C#'s `:`, are fixed to `is`, and Tutorial
  D's `IS { PLANE_FIGURE POSSREP … }` is reported (P0043). A union type
  can be a subtype of another, as in `type curve union is plane_figure;`,
  but of no other type, and a built-in type has no subtypes (A0060).
- **A union type has two subtypes at least,** since with one it would
  have that one's values, and with none no values (A0060). It isn't a
  subtype of itself, directly or through others, and contains each of
  its subtypes, so none of them has a component of it (A0049).
- **Subtypes are disjoint:** a value carries its type, so no ellipse is
  a polygon, and a value of the union type is of exactly one of its
  subtypes.
- **A union type has no default and no order,** so a variable of the program's
  of one needs an initial value (A0006), a local variable has none until a
  statement gives it one (A0054), and `<` is an error (A0022). `load_csv`
  reads a subtype's values as any type's, but not a union type's, since
  a record's columns can't say which subtype it is.
- **A subtype's value is a value of its union type,** so it can be given
  wherever one of the union type is expected: assigned to a variable,
  passed to an operator, or held in an attribute or an element (IM
  Prescription 16). It is the same value, so nothing is converted.
- **Values that meet have the most specific type they all are** (IM
  Prescription 8): `figures` above is a `relation { name: text, figure:
  plane_figure }`, and so is an array of an ellipse and a polygon, or an
  `if` that gives one or the other.
- **What is known of a value is its declared type:** `figure` holds an
  ellipse, but `figure.a` is an error, since a plane figure needn't be an
  ellipse (A0022), and so is giving it where an ellipse is expected
  (A0004), whether or not the compiler knows the value.
- **`=` and `<>` compare values of types that overlap** (IM Prescription
  12): `figure = lake` compares two plane figures, whichever is written
  first, but `lake = field` is an error, since no ellipse is a polygon
  (A0004).
- **A `var` argument is of exactly its parameter's type** (IM
  Prescription 19), since the operator can give the variable any value
  of that type, which a variable of a subtype couldn't hold, and reads
  what it holds, which a variable of a supertype could hold a value of
  another type than (A0004). An `out` argument can be of a supertype of its
  parameter's type: the operator never reads it, and what it gives it as it
  ends is one of the variable's values, as an assignment of it would be
  (RM Prescription 3). So `var f: figure; call make{out c: f};` gives `f` a
  circle.

`x is t` tells the subtypes apart: it is true if the value of `x` is of
the type `t`, or of one of its subtypes (IM Prescription 15). In the rest
of the condition it is in, `x` is a `t`, so its components can be read:

```trendy
type plane_figure union;
type ellipse is plane_figure { { a: approximate, b: approximate } };
type polygon is plane_figure { { corners: integer } };

var figures := relation {
    { name: "Victoria", figure: ellipse{a: 3e0, b: 2e0} },
    { name: "Zócalo", figure: polygon{corners: 4} },
};

/* relation { name: text, figure: ellipse } */
var ellipses := figures where figure is ellipse;
var wide := figures where figure.a > 2e0 and figure is ellipse;

operator describe{f: plane_figure}: text [
    if f is ellipse then "an ellipse " || to_text{from: f.a}
    else if f is polygon then to_text{from: f.corners} || " corners"
    else "a plane figure"
];

operator main{} [
    call debug_table{r: ellipses extend { width := 2e0 * figure.a }};
    call debug_value{v: describe{f: polygon{corners: 3}}};
    call debug_value{v: treat value from (figures where name = "Victoria"){figure} as ellipse};
];
```

- **A test narrows its name where the condition needs it to be true:**
  in the other operands of an `and`, after `implies`, in an `if`'s `then`
  branch, and in a `where`'s condition. `not` turns it round, as `else`
  does: in `not (f is ellipse) or f.a > 2e0`, and in the `else` of `if
  not (f is ellipse)`, `f` is an ellipse. A name only narrows: `x` in `x
  is t`, not a longer expression.
- **A test that is false shows nothing:** where `f is ellipse` is false,
  `f` isn't known to be a polygon, though `plane_figure` has no other
  subtype yet, since a program can add one. So `describe` above tests for
  each kind it reads the components of, and has a branch for the rest,
  and reading `f.corners` in an `else` after `f is ellipse` is an error
  (A0022), as it is in TypeScript's closed unions.
- **The order doesn't matter:** `figure.a > 2e0 and figure is ellipse`
  is `wide` above too. A narrowed name's value is treated as one of the
  type, which would fail if the test were false, but a false test decides
  the `and` whatever the rest does (see [Boolean
  operators](#boolean-operators)), and the test, which can't fail, is
  computed first.
- **A `where` narrows its result's heading:** `ellipses` is a relation
  of ellipses, so `figure.a` can be read in its `extend` with no test of
  its own.
- **A test decides something** (A0062): one always true, as `e is
  plane_figure` of an ellipse, or never, as `e is polygon`, is an error.
  So is a test of a tuple's, a relation's or an array's type, which is
  known when compiling: `t.figure is ellipse` tests what is in it.
- **`treat x as t` is the value of `x` as one of `t`,** a subtype of its
  type, which fails unless it is one (IM Prescription 14): known when
  compiling, it is reported then, and otherwise the program stops (R0019).
  It says what a test can't show, and is seldom needed: where a value may
  be of another type, test it instead. One that always succeeds, or never,
  is an error (A0062).
- **Other languages' tests and casts are fixed** (P0044, A0015): Java's
  `x instanceof t`, Python's `isinstance(x, t)` and SQL's `x IS OF (t)`
  to `x is t`, SQL's `TREAT(x AS t)` to `treat x as t`, and Tutorial D's
  `IS_T(x)` and `TREAT_AS_T(x)` to them.

In an update operator, an `if` statement's branches are narrowed as an
`if`'s are, and so is a `while` loop's block by its condition, which is
true where the block starts. A test shows what a value is only until a
statement that may change it:

```trendy
type plane_figure union;
type ellipse is plane_figure { { a: approximate, b: approximate } };
type polygon is plane_figure { { corners: integer } };

operator widen{var f: plane_figure} [
    if f is ellipse then [
        call debug_value{v: f.a};
        update f { a := 2e0 * a };
        call debug_value{v: f.a};
    ] else if f is polygon then [
        call debug_value{v: f.corners};
        f := polygon{corners: f.corners + 1};
    ];
];

operator main{} [
    var lake: plane_figure := ellipse{a: 3e0, b: 2e0};
    call widen{var f: lake};
    call debug_value{v: lake};
];
```

- **A statement that may assign a name ends its narrowing,** after it:
  an assignment, an `insert` or a `delete` of it, a `load` into it, or a
  `var` or `out` argument of it, in the statement or in one inside it. A
  call may assign any variable of the program's, so it ends theirs. After
  one, reading a component is an error that points at it (A0022).
- **`update f { … }` keeps it:** it is `f := f replace { … }`, whose `f`
  is narrowed, so it changes the components of the type the test shows,
  and `f` holds a value of that type still.
- **A loop runs its block again,** after what it assigns, so a name its
  block may assign isn't narrowed anywhere in it, nor in its condition.
- **A `var` or `out` argument passes the variable,** of its declared
  type, which the operator can give any value of it: `call widen{var f:
  lake}` passes `lake`, a `plane_figure`, wherever a test narrows it.

An operator of a union type's values can be computed differently for
each of its subtypes, by versions of it (IM Prescription 17): operators
of its name and its parameters' names whose parameters are of subtypes
of its own. A call names the operator, as any call does, and computes
the most specific version whose parameters its arguments' values are of,
when compiling if they are known then, and otherwise when running:

```trendy
type plane_figure union;
type ellipse is plane_figure { { a: approximate, b: approximate } };
type polygon is plane_figure { { corners: integer } };

/* No body: a version for each kind of figure computes it. */
operator sides{f: plane_figure}: integer;
operator sides{f: ellipse}: integer [ 0 ];
operator sides{f: polygon}: integer [ f.corners ];

/* A body for the rest, and a version for ellipses. */
operator name{f: plane_figure}: text [ "a plane figure" ];
operator name{f: ellipse}: text [ "an ellipse" ];

operator main{} [
    var figures := relation {
        { figure: ellipse{a: 3e0, b: 2e0} },
        { figure: polygon{corners: 4} },
    };
    call debug_table{r: figures extend { sides := sides{f: figure}, name := name{f: figure} }};
];
```

- **The operator is the specification,** the one whose parameters' types
  are supertypes of all the others'. Without a body, `;` in its place, it
  has a version for each type a value can be of, which is reported at the
  type that has none (A0063): the program that adds a subtype to a union
  type adds its versions. With a body, that computes it for the rest.
- **A version is like its specification:** read-only exactly if it is,
  with each parameter taken as it takes it, and a result of its result's
  type or a subtype of it (A0063). A `var` or `out` parameter has its
  specification's type, since the operator can give the variable any
  value of it.
- **One version is the most specific for any arguments:** two that are
  both for some, neither more specific than the other, are an error
  (A0063), as are two of the same types, or two of one name and
  parameters with no specification of both, which the error writes
  (A0001, A0063). A version has no name of its own: Tutorial D's
  `OPERATOR AREA VERSION AREA_C …` is fixed (P0045).
- **A tuple, a relation or an array is of a version's type by what it
  holds** (IM Prescription 27): a tuple if each attribute's value is of
  its type, and a relation or an array if each tuple or element is, which
  a call tests of each one. So an empty one is of every relation or array
  type of its heading or elements, and computed by the most specific
  version, and two versions for relations of disjoint types, as
  `relation { f: ellipse }` and `relation { f: polygon }`, are both for
  it, which no version can be more specific than (A0063).

Relations whose attributes are of different types in one hierarchy
combine (IM Prescription 13). A union's attribute is of the most specific
type both operands' are, since it has the values of both; an
intersection's or a join's common attribute is of the more specific one,
since a value in both is of both; and a difference's or a `matching`'s
attributes are the left operand's, whose tuples the result's are:

```trendy
type plane_figure union;
type ellipse is plane_figure { { a: approximate, b: approximate } };
type polygon is plane_figure { { corners: integer } };

var ellipses := relation { { name: "pond", figure: ellipse{a: 3e0, b: 2e0} } };
var polygons := relation { { name: "plot", figure: polygon{corners: 4} } };

/* A relation of plane figures. */
var figures := ellipses union polygons;

/* Of ellipses, so `figure.a` can be read. */
var majors := (figures intersect ellipses) extend { major := figure.a }{name, major};

/* Of plane figures, the plot. */
var others := figures minus ellipses;

operator main{} [
    call debug_table{r: majors};
];
```

- **Matched and compared attributes overlap:** a join's, a `matching`'s
  and `!!`'s common attributes, and an intersection's or a difference's
  attributes, are of one type, or of types one of which is a subtype of
  the other, so that a value can be in both. `ellipses join polygons` is
  an error, since no ellipse is a polygon (A0027), and so is
  `ellipses minus polygons`, which would be `ellipses` whatever their
  values (A0031).
- **A union's attributes have a common supertype:** `ellipses union
  polygons` is of plane figures, but a union of ellipses and integers is
  an error (A0031).
- **`i_delete r s` deletes tuples of overlapping types,** as
  `r i_minus s` does, while `insert r s` and `d_insert r s` insert those
  of `r`'s types or their subtypes, since `r` keeps its type.

### Subtypes defined by constraints

A subtype of a type with possible representations is defined by its
constraints, conditions on the supertype's components: its values are
those of the supertype they are true of (*Type Inheritance and
Relational Theory*, chapter 8, IM Prescription 10). A circle is an
ellipse whose semiaxes are equal:

```trendy
type ellipse { { a: approximate, b: approximate, ctr: integer }, constraint a >= b };
type circle is ellipse {
    constraint a = b,
    { r: approximate, ctr: integer }
        to { a := r, b := r, ctr := ctr }
        from { r := a, ctr := ctr },
};

operator kind{e: ellipse}: text [ "an ellipse" ];
operator kind{e: circle}: text [ "a circle" ];

operator stretch{var e: ellipse} [
    update e { a := 2e0 * a };
];

operator main{} [
    var e := ellipse{a: 2e0, b: 2e0, ctr: 0};
    call debug_value{v: kind{e: e}};
    call stretch{var e: e};
    call debug_value{v: kind{e: e}};
    var c := circle{r: 3e0, ctr: 1};
    call debug_value{v: c.r};
];
```

- **Selecting a value gives it its most specific type,** specialization
  by constraint: `ellipse{a: 2e0, b: 2e0}` is a circle, though it is
  declared an ellipse, and `is`, `treat`, narrowing and versions see it
  as one. Selecting tests each subtype's constraints, and its subtypes'
  in turn, when compiling if the components are known then, and
  otherwise when the program runs.
- **An update selects it again,** generalization by constraint:
  `stretch` makes the circle just an ellipse, and an update can make an
  ellipse a circle. A variable of type `circle` holds circles only, so an
  update that makes its value just an ellipse fails, as one that breaks
  any constraint does (R0017). An `update` of a name a test narrows to a
  circle changes it as an ellipse, and it is narrowed to an ellipse
  after.
- **Its values are its supertype's:** a circle has an ellipse's
  components, read as `c.a`, and its constraints read them. It needs a
  constraint, and has no order of its own: its values are ordered as its
  supertype's are (A0065). It is in its supertype's hierarchy, as a
  union type's subtype is, and its constraints are its supertype's too.
- **Its own representations convert to its supertype's components:**
  `{ r, ctr }`, as TTM derives a circle's possible representation from an
  ellipse's, converts `to` them, computing them from its own, and `from`
  them, computing its own, as any representation but the stored one does
  (A0065). So `circle{r: 3e0, ctr: 1}` selects a circle, which fails
  unless its constraints are true of the components `to` gives (R0017),
  `c.r` reads its radius, and `update c { r := … }` changes it. A
  component of its supertype's, as `ctr`, is the same one if both
  conversions carry it unchanged, `ctr := ctr`; any other is a component
  of two representations (A0008).
- **A value has one most specific type,** so a value of two subtypes of
  one type, neither a subtype of the other, is an error when it is
  selected (R0020): their constraints must never both be true.
- **Written in the stored representation,** the one the others convert
  to, as every value is: a circle is written as the ellipse it is,
  `ellipse{a: 3e0, b: 3e0, ctr: 1}`, with the selector of the type that
  declares that representation, which selects the circle again, and `is
  circle` tells whether a value is one. A subtype's own representations
  aren't its supertype's, so there is no `circle{a: …, b: …, ctr: …}`.

## Tuples

A tuple type is written `tuple { name: type, … }`, and a tuple value
`tuple { name: value, … }`, or `tuple { name, … }` for a name's own value (see
[A name alone](#a-name-alone)). Tuples are values like any other: an attribute
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
- **Only relation variables have keys,** and a type's relation-valued
  components, below, not tuple or scalar variables (A0020), nor relation
  types or values: a key is a rule about what a variable, or a type's
  values, can hold.
- **Every value must satisfy the keys.** Two tuples with the same values
  for a key are an error (R0008). An initial value is checked when
  compiling, and the error is reported at the two tuples; an assigned
  value is checked when the assignment is made, or, in an `assign` block,
  once all of the block's are (see [Assignment](#assignment)), and a value
  known only when the program runs stops it, writing the variable, the
  key and the two tuples. Of several, they are the first tuple, in
  canonical order, to have the same values for the key as one before it,
  and the first of those, when compiling as when running.
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

A type of the program's own can declare keys of its relation-valued
components, each written after the component's type, as a relation
variable's are after its own. They are rules of the type: no value of it
has a component with two tuples that share a key's values, which, as its
constraints are, is checked whenever a value is selected, by its
selector, `replace`, `update` or `load_csv` (R0008), when compiling if the
components are known then, and otherwise when the program runs:

```trendy
type outline {
    { vertices: relation { v_no: integer, x: approximate, y: approximate } key { v_no } key { x, y } },
};

var triangle := outline{vertices: relation {
    { v_no: 1, x: 0e0, y: 0e0 },
    { v_no: 2, x: 4e0, y: 0e0 },
    { v_no: 3, x: 0e0, y: 3e0 },
}};

operator main{} [
    call debug_value{v: triangle};
];
```

```trendy,error
type outline { { vertices: relation { v_no: integer, x: approximate } key { v_no } } };

var line := outline{vertices: relation { { v_no: 1, x: 0e0 }, { v_no: 1, x: 4e0 } }};

operator main{} [];
```

- **The rules of a variable's keys hold:** a key lists attributes of the
  component's relation, none contains another (A0025), and `key {}`
  allows at most one tuple. A component that isn't a relation has no
  keys (A0020).
- **A component of any representation** can have keys, as a constraint
  can name its components: one of a representation other than the stored
  one is computed by its conversion when a value is selected.
- **A key goes after its component's type:** one among the type's
  constraints and order is fixed to after the type of the one component
  with its attributes (A0064).
- **A relation type has no keys:** its values are all the relations of
  its heading, so a key of an attribute in a heading, or of a parameter,
  is an error (P0046). To key relations of a heading wherever they are,
  make them a type's component, and to key facts, keep them in a relation
  variable.
- **A value taken from a component is a relation like any other:**
  `var vs := triangle.vertices` has the keys `vs` declares, and none else.

```trendy,error
var shapes: relation { name: text, vertices: relation { v_no: integer } key { v_no } };

operator main{} [];
```

## Constraints

A constraint is a condition on a unit's variables, named, which holds
whenever a statement ends: no statement leaves the variables in a state
that breaks it. It is TTM's database constraint, of the unit's own
variables (RM Prescription 23), as a key is a constraint on one relation
variable, written with it. A constraint's name is defined with `:=`, as
a variable's is, and says which constraint is broken, when one is.

```trendy
var countries := relation {
    { country: "Kenya", native: "Kenya" },
    { country: "Peru", native: "Perú" },
}
key { country };
var languages := relation {
    { language: "Swahili", native: "Kiswahili" },
    { language: "Quechua", native: "Runa Simi" },
}
key { language };
var official := relation {
    { language: "Swahili", country: "Kenya" },
    { language: "Quechua", country: "Peru" },
}
key { language, country };

/* Each language is official in a country there is, and somewhere. */
constraint known_countries := official{country} <= countries{country};
constraint known_languages := official{language} <= languages{language};
constraint official_somewhere := languages{language} <= official{language};

/* A language, with where it is official, at once, as neither holds without
   the other. */
operator add_quechua{} [
    assign {
        insert languages relation { { language: "Quechua", native: "Runa Simi" } };
        insert official relation { { language: "Quechua", country: "Peru" } };
    };
];

operator main{} [
    call add_quechua{};
    var refused: relation transaction_collection;
    transaction collecting refused [
        insert languages relation { { language: "Esperanto", native: "Esperanto" } };
    ];
    call debug_value{v: refused{error}};
];
```

- **A condition on variables,** of any type, which it mentions one or
  more of. It is a boolean expression, so it calls read-only operators
  only. One that mentions no variable says nothing about them, or can
  never hold, and is an error (A0058), whose fix deletes it when it is
  always true.
- **The initial values satisfy it.** They are known when compiling, so a
  constraint they break is reported then (R0023), with what breaks it
  where its form says: the tuples of `a` that aren't in `b`, for
  `a <= b`, and of `b` that aren't in `a`, for `a >= b`; the tuples of
  `e`, for `is_empty{r: e}`; and the operands of an `and` that are false.
  One the compiler can't compute of them, as it reads a constant computed when
  the program runs (see [Constants](#constants)) or calls an operator that
  runs statements, is checked as the program starts, before `main`, and fails
  as a statement that breaks it does, at its declaration.
- **Every statement that assigns a variable it mentions is checked as it
  ends:** an assignment, an `assign` block, a call that passes the
  variable to a `var` or `out` parameter, and a transaction that adds its
  errors to it. Its parts, the operands of its `and`s, are checked in
  turn, those that can't fail first, as an `and` computes them, and a
  statement that leaves one false fails (R0023), with a
  `constraint_violated { constraint, condition }`, or, where its form says
  which tuples make it false, a `constraint_violated_by { constraint,
  condition, tuples }`, with the relation of them as `debug_value` writes
  it. A collecting transaction takes the failure, and gives the variables
  back their values, so no statement after it sees them broken; otherwise
  the program stops. Changes that only hold together are made in one
  statement, an `assign` block, as `add_quechua` below makes them.
- **A unit's own.** A constraint is on its unit's own variables, and one
  on another unit's is an error (A0083), as it would fail that unit's
  statements by a condition the unit never sees. One on a variable the
  interface declares is the interface's, where every unit that assigns the
  variable sees it, and one in the implementation is moved there (A0067);
  one in the interface mentions only those, as the interface sees only
  interfaces. A constraint is a definition, at the top level, so one in an
  operator's body is moved before it (P0034).
- **Names of their own:** no name refers to a constraint, so one can have
  a variable's or an operator's name, but no two constraints can share
  one (A0001).
- Tutorial D's `CONSTRAINT name condition`, without `:=`, `name:
  condition`, and SQL's `CREATE ASSERTION name CHECK (condition)` are
  fixed to `constraint name := condition` (P0055). A constraint without a
  name is an error with help, since its name is the program's to give.

```trendy,error
var countries := relation { { country: "Kenya" } } key { country };
var official := relation {
    { language: "Swahili", country: "Kenya" },
    { language: "Quechua", country: "Peru" },
}
key { language, country };

constraint known_countries := official{country} <= countries;

operator main{} [];
```

```trendy,error
var countries := relation { { country: "Kenya" } } key { country };
var official := relation { { language: "Swahili", country: "Kenya" } } key { language, country };

CONSTRAINT known_countries official{country} <= countries;

operator main{} [];
```

## Databases

A database is a variable that outlives the program, whose value is its
relvars together: TTM's database (RM Prescription 16), which Appendix D
calls a dbvar, a tuple variable with a relation-valued attribute for each
relvar. `database kitchen;` declares one, and its units add to it, each
relvar and each constraint declared on its own, as Tutorial D declares
them, with the database's name before its own:

```trendy
interface

database kitchen;

var kitchen.pantry: relation { item: text, grams: integer } key { item };
var kitchen.recipes: relation { recipe: text, item: text } key { recipe, item };

/* Nothing in the pantry weighs less than nothing, and a recipe needs only
   what the pantry keeps. */
constraint kitchen.stocked := is_empty{r: pantry where grams < 0};
constraint kitchen.known := recipes{item} <= pantry{item};

implementation

operator main{} [];
```

- **Only relvars:** a database holds relations, each a relvar with its
  type written and its keys, and no initial value, as its value is the
  database's, which outlives every program; a fix removes one (A0070).
- **Its constraints mention its relvars alone,** named alone in them,
  without the database's name: a database's constraint holds for every
  program that uses the database (RM Prescription 23), which none could
  check of another program's variables (A0070). As a unit's constraints,
  one that mentions none is A0058.
- **Names:** a database is a variable, so it shares the variables'
  namespace (A0001). Its relvars and constraints are named within it, so
  a relvar can have a program variable's name, or another database's
  relvar's.
- **In the interface:** a unit's interface may declare a database and add
  to it, as what is shared and outlives the program, while its other
  variables stay its implementation's. A unit that only adds to one has
  nothing else to give a program, so that is what gives it a use, and the
  fix for one without `interface` writes it (A0002).
- **Only its own units add to it:** the unit that declares it, and the
  units under its directory, as `library/loans.trendy` is under
  `library.trendy`'s (A0070, the catalog's `own_parts`). The compiler
  reads them all, used or not, so that every program that uses the
  database has all of its constraints, which hold for each of them (RM
  Prescription 23). A program's own data that outlives it goes in a
  database of its own.
- **Declared for its definition:** no statement reads a database yet
  (A0070).
- SQL's `CREATE DATABASE name` is fixed to `database name;`, and Tutorial
  D's `PRIVATE` before a relvar's type removed; SQL's `CREATE SCHEMA`, and
  Tutorial D's `REAL`, `BASE` and `PUBLIC` relvars, are reported with help
  (P0056).

```trendy,error
database kitchen;

var kitchen.pantry: relation { item: text, grams: integer } key { item };
var kitchen.count: integer;

constraint kitchen.stocked := is_empty{r: kitchen.pantry where grams < 0};

operator main{} [];
```

### The catalog

The compiler describes each program in a database of its own, the
catalog, which the unit `trendy.catalog` declares, as a database describes
itself (TTM RM Prescription 25). The compiler fills its relvars as it
checks the program's units, and only it assigns to them. Their
constraints are the rules between units: a program that breaks one
doesn't compile, and the tuples that break it say where, as the error for
a unit that uses another there isn't is made from the tuples of
`uses{used}` that `units` hasn't.

```
/* The catalog: what the compiler knows of a program's units, which it
   describes as it compiles them, as a database describes itself (TTM RM
   Prescription 25). The rules between units are its constraints: a
   program that breaks one doesn't compile, and the tuples that break it
   say where. Only the compiler assigns to it. */
interface

database catalog;

/* Each unit of the program, by its name: main's, those it uses, and
   those that come with the compiler. */
var catalog.units: relation { unit: text } key { unit };

/* Each unit a unit's section uses, its `interface` or its
   `implementation`. */
var catalog.uses: relation { unit: text, used: text, section: text };

/* Each database, by its name in the program, and the unit that declares
   it. */
var catalog.databases: relation { unit: text, database: text };

/* Each relvar or constraint a unit adds to a database, by its name in it. */
var catalog.parts: relation { unit: text, database: text, part: text };

/* Each unit, with itself and with each unit whose directory it is under,
   its owners: `library.loans`, of the file `library/loans.trendy`, is
   under `library`. */
var catalog.within: relation { unit: text, owner: text };

/* A unit uses units of the program. */
constraint catalog.known_units := uses{used} <= units rename {unit as used};

/* No unit uses itself, through any path. */
constraint catalog.acyclic := is_empty{r: uses{unit, used} tclose where unit = used};

/* Only a database's own units add to it: the one that declares it, and
   those under its directory. */
constraint catalog.own_parts := (parts join (databases rename {unit as owner})){unit, owner}
    <= within;
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

- **`a || b` is the elements of `a`, then those of `b`,** of one array type, as `||` is
  of text.
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
  `count` and `length` given the other's kind of value to each other, or
  a text, to `length` of its code points (A0015, A0004); and `t["name"]`
  for a tuple's attribute, to `t.name`
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
uses { trendy.files };

operator main{} [
    var texts: array text;
    call load_text_file{path: file_path{text: "rivers.txt"}, out lines: texts};
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

- **`insert` changes the variable's own buffer where it can,** as a value
  nothing else holds can't be told from a new one. So adding a few tuples takes
  no copy of the others, whichever is written, `insert r s;` or
  `r := r union s;`, and one that goes last takes one comparison, however many
  there are; a tuple that goes elsewhere moves those after it. A value another
  variable, a parameter or a transaction holds doesn't change: the first
  `insert` makes the variable a copy of its own. So does an `insert` with other
  changes at once, in an `assign` block, as they read the variable as it was.
  Only what it adds is checked against its keys.
- **So do `delete` and `i_delete`,** written so or as `r := r where p;` and
  `r := r minus s;`. What goes is found before anything does, so a condition
  reads the relation whole, and one that fails changes nothing. A condition
  that says what the first attributes equal, as `delete r where k = x;` does,
  finds its tuples rather than going through them all (see
  [Restriction](#restriction)), so that it takes the time of moving those after
  them, and none if they are last.
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

`not`, `and`, `or` and `xor` are also of integers, whose bits they take
(see [Bits](#bits)), but there is no truthiness: `"" or true` is an
error, and so is `n and true` of an integer `n`. Of operands of both
types, the boolean ones set it, and an integer among them is reported,
as a boolean written as C writes it is likelier than bits.

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
- **Any operand that decides the result decides it.** In a chain of
  `and`s, `a and b and c`, however it is grouped, or `and{a, b, c}`, an
  operand that is `false` makes the result `false`, whatever the others
  do, failing included, so they needn't be computed; the result fails only
  if no operand is `false` and one fails, as the first written that fails
  does. `or` is the same with `true`, and `implies` is `not a or b`. So
  `y div x > 1 and x <> 0` is `false` where `x` is zero, wherever the `x
  <> 0` is written, unlike in languages that decide only from the left, or
  SQL, which doesn't say. As nothing depends on which operand is computed
  first, the program chooses, computing each only until one decides the
  result: those that can't fail first, which are cheap, then the others.
  In a restriction, those of the others that call operators are computed
  in the order likeliest to decide it soonest for what each costs, which
  the program learns as it goes through the tuples, and keeps for the next
  time the restriction runs; elsewhere, in the order written.
- **An expression can fail** if it does arithmetic, `+`, `-`, `*`, `/`,
  `div`, `mod`, `shl`, `shr` or a prefix `-` of an integer, which can
  overflow or divide by zero; joins texts, `||`, or builds, changes, combines or loads a
  relation or an array, which can run out of memory; takes an element or
  the only tuple; selects a value of a type with constraints, keys of
  components or subtypes defined by constraints, which it checks; orders
  values, with `<`, `<=`, `>` or `>=`, of a type ordered in another
  representation than the stored one, which converts them, or of a type
  parameter's type, which may be given such a type; or calls an operator that
  can fail. Comparisons, `not`, `and`, `or`, `xor` and `implies`, of booleans
  or of integers, literals, names, attributes and tuple selectors can't fail
  themselves; an expression made of them can fail only if a part of it can.
- **An operator can fail** if it is a built-in other than `count`, `length`,
  `to_approximate`, `sin`, `cos` and `atan2`, which have a value for any
  argument, without making anything; or if its value can fail, or one of its
  versions can, or its parameters have constraints, which a call checks; or if
  it calls itself, directly or through others. A call needs no more memory than
  the compiler knows of, so one of an operator that can't fail can't run out of
  it, but a chain of calls that leads back to the same operator can go on until
  there is no more (R0025). So `length{a: jumps}`, `is_empty{r: r}` and
  `within{n: n, size: size}`, where `within` is `n >= 1 and n <= size`, can't
  fail, and an `and` computes them as it does a comparison.

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
overflow: 9223372036854775807 + 1 is out of its type's range`, and where
it was (see [Programs](#programs)), and exits with status 1, unless a
transaction takes the failure's error, an `overflow` or a
`division_by_zero` (see [Transactions](#transactions)).

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
`pi` is the approximate number nearest π, a constant (see
[Constants](#constants)). `sqrt` is correctly
rounded, as IEEE 754 requires; the others are computed the same way on
every machine, so a result is the same whether the compiler computes it or
the program does. A negative number has no square root (R0018), which is
a compile error when it is known while compiling, and otherwise stops the
program as an overflow does.

```trendy
var circumference := 2e0 * pi * 1.5e0;
var hypotenuse := sqrt{of: 3e0 * 3e0 + 4e0 * 4e0};

operator polar{x: approximate, y: approximate} [
    call debug_value{v: sqrt{of: x * x + y * y}};
    call debug_value{v: atan2{y: y, x: x}};
    call debug_value{v: cos{of: pi / 3e0}};
];

operator main{} [
    call polar{x: 1e0, y: 1e0};
];
```

```trendy,error
var root := sqrt{of: -4e0};

operator main{} [];
```

### Bits

`not`, `and`, `or` and `xor` of integers, and of bytes (see [Bytes](#bytes)), take
their bits, as two's complement writes an integer's, the sign's bit going on to the left forever, so
that a result doesn't depend on how wide an integer is: `not a` is
`-a - 1`. `shl` and `shr` move the bits, as they multiply and divide by
a power of 2.

| Operator | Operands | Result |
|---|---|---|
| `not a` | an integer | each bit flipped: `-a - 1` |
| `a and b`, `a or b`, `a xor b` | integers | the bits set in both, in either, and in exactly one: `0b1100 and 0b1010` is `0b1000` |
| `and{a, b, c}`, `or{a, b, c}`, `xor{a, b, c}` | integers | the same, of any number; of none, `and{}` is `-1`, every bit set, and `or{}` and `xor{}` are `0` |
| `a shl n` | integers | `a` times 2 to the power `n`, its bits moved `n` places to the left |
| `a shr n` | integers | `a` divided by 2 to the power `n`, rounded down, its bits moved `n` places to the right: `-9 shr 1` is `-5` |

- **The same precedence as of booleans:** `not` binds tightest, and
  `and`, `xor` and `or` looser than a comparison, so `flags and 4 = 4` is
  `flags and (4 = 4)`, as `flags & 4 == 4` is in C. Of an integer and a
  comparison of integers, the comparison of bits was surely meant, and
  the fix parenthesizes it, `(flags and 4) = 4` (A0076). `shl` and `shr`
  bind as `*` does: `1 shl n - 1` is `(1 shl n) - 1`.
- **`not` of an integer that begins a comparison** is written in
  parentheses, since SQL and Python read `not n = 0` as `not (n = 0)`,
  and C `~n == 0` as `(~n) == 0`: neither is assumed (A0076).
- **A shift never loses a bit but by rounding down.** A left shift out of
  range fails, as an overflow does (R0001), so `1 shl 63` is an error,
  never the smallest integer. A negative `n` shifts the other way, as
  Haskell's `shift` does, and beyond 63 places a right shift leaves `0`,
  or `-1` of a negative integer.
- **A word of fixed width** is the bits it keeps, `and` a mask, as in
  `(x shl n or x shr (32 - n)) and 0xFFFF_FFFF`, a 32-bit word rotated.
  There is no unsigned shift, as JavaScript's `>>>`, since no integer is
  unsigned: masking first, `(a and 0xFFFF_FFFF) shr n`, is one.

```trendy
var mode := 0o755;
var readable := (mode and 0o444) <> 0;
var flags := 0b0101 or 0b0010;

/* `x`, a 32-bit word, its bits rotated `n` places to the left. */
operator rotated{x: integer, n: integer}: integer [
    (x shl n or x shr (32 - n)) and 0xFFFF_FFFF
];

operator main{} [
    call debug_value{v: rotated{x: 0x8000_0001, n: 4}};
    call debug_value{v: not flags};
    call debug_value{v: -9 shr 1};
];
```

```trendy,error
var flags := 6;
var set := flags and 4 = 4;

operator main{} [];
```

```trendy,error
var smallest := 1 shl 63;

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

A text is made of *code points*, values of the prelude's `code_point`:
`to_code_points{from: t}` is the array of the text `t`'s, in order, and
`from_code_points{from: a}` the text of the array `a`'s.

```trendy
var word := "Paraná";

operator main{} [
    var points := to_code_points{from: word};
    call debug_value{v: points[6]};
    call debug_value{v: points[6].character};
    call debug_value{v: length{a: points}};
    call debug_value{v: points[1] = code_point{character: 'P'}};
    call debug_value{v: from_code_points{from: points} = word};
];
```

- **A code point is a Unicode scalar value,** as `code_point{value: 225}`
  is `á`: a number from 0 to 10FFFF hex, but not the surrogates, D800 to
  DFFF, which no UTF-8 text holds, so selecting another fails (R0017). It
  is a type of its own, not an integer, as *The Third Manifesto*'s `QTY`
  is (chapter 6): its number is `p.value`, and code points are ordered by
  it. So every array of them has a text, which `from_code_points` gives.
- **It is the character of it alone too,** another possible representation,
  `p.character`: `code_point{character: 'á'}` is `code_point{value:
  225}`, and, of literals, a literal itself, as every selection of them
  is. Its own constraint says the character is one code point's (see [A
  program's own types](#a-programs-own-types)), so
  `code_point{character: '🇸🇪'}` fails (R0017).
- **It is less than a character a reader sees,** which may be several (see
  [Characters](#characters)). `é` can be written as one code
  point, `\u{E9}`, or as two, `e` and a combining accent, `\u{301}`, which
  are different texts.
- **Each is made as selecting it would make it,** so of a subtype a
  constraint defines, as `type ascii is code_point { constraint value <
  128 };` does, if it is true of it (see [Subtypes defined by
  constraints](#subtypes-defined-by-constraints)).
- **A text's length is the count of its code points,** `length{a:
  to_code_points{from: t}}`, as Python's `len` counts them, which
  `length` or `count` of a text is fixed to (A0004).
- **Other languages' habits are fixed:** Rust's `chars` and Java's
  `codePoints` to `to_code_points`, JavaScript's `String.fromCodePoint`
  to `from_code_points`, Python's `ord{c}` to `code_point{character:
  c}.value`, and its `chr{n}`, as JavaScript's `String.fromCharCode` of
  one number, to `code_point{value: n}.character` (A0015); JavaScript's
  `charCodeAt`, of a position, is reported with how to write its value.

```trendy,error
var word := "Zürich";
var letters := length{a: word};

operator main{} [];
```

### Characters

A `character` is one character, what a reader sees as one, however many code points
it is made of: `'a'`, `'é'`, the flag `'🇸🇪'`, of two, or the family `'👨‍👩‍👧‍👦'`, of seven. It is
written between single quotes, as text is between double ones, with the same escapes,
and `\'` for its own quote: `'\n'`, `'\u{1F44D}'`, `'\''`.

```trendy
const greeting := "नमस्ते 🇳🇵";

operator reversed{t: text}: text [
    var result := "";
    for c in to_characters{from: t} loop [
        result := from_character{from: c} || result;
    ];
    result
];

operator main{} [
    call debug_value{v: to_characters{from: greeting}};
    call debug_value{v: length{a: to_characters{from: greeting}}};
    call debug_value{v: length{a: to_code_points{from: greeting}}};
    call debug_value{v: reversed{t: "🇸🇪🇫🇮"}};
    call debug_value{v: 'é' = 'e\u{301}'};
    call debug_value{v: min{r: relation { { c: 'b' }, { c: 'a' } }, of: c}};
];
```

- **One character is an extended grapheme cluster** of Unicode's UAX #29, by its
  default rules, at Unicode 18.0.0, whatever the locale, so that it depends on its code
  points alone: Slovak's `ch` is two. Those rules change between versions of Unicode,
  and a type is a set of values (*The Third Manifesto*, chapter 3), so the version is
  part of the language, and changes only with it.
- **Literals say which at once.** `'abc'` is an error, with a fix to `"abc"`, as is
  `''` to `""`, `'''` to `'\''`, and `'🇸🇪🇫🇮'`, two flags (P0060). `'a'` where a
  text is expected, as SQL, Python and JavaScript write text, and `"a"` where a
  character is, are fixed to each other (A0004).
- **It shows no representation.** It has no components and no selector, only literals,
  as *The Third Manifesto* allows a built-in type (chapter 6), and it converts only
  through operators. `from_character{from: c}` is its text alone; `to_text{from: c}`
  writes it as source, `'a'`, as `to_text` writes any value. `parse_character{from: t}`
  is the character `t` is, and fails unless it is exactly one (R0014). `||` and
  `length` take text and arrays, and `c = "a"` is an error, as a character isn't a text.
- **`to_characters{from: t}` is the array of `t`'s characters, in order, and
  `from_characters{from: a}` the text of the array `a`'s.** The text of a text's
  characters is that text. The characters of an array's text needn't be that array, as
  two can join: `'🇸'` and `'🇪'` are one flag, written together.
- **Two are equal only if they are the same code points,** as equal values are the same
  value, which no operator can tell apart (RM Prescription 8), and
  `to_code_points` can tell these. So `'é'` written as one code point isn't `'e\u{301}'`,
  as `"é"` isn't `"e\u{301}"`: nothing normalizes. Characters are ordered by
  their code points, as text is. There is no collation, as there is none of text.
- **It has no default,** as no character is any more a start than another, and none
  is empty: `var c: character;` is an error of the program's variables (A0006), and a
  local variable has no value until a statement gives it one (A0054), but a relation or
  an array of them starts empty, as any does.

### Bytes

A `byte`, a type of the prelude, is a number from 0 to 255, written `x'7f'`, two
hexadecimal digits between `x'` and `'`. An array of them, `array byte`, is what binary
data is, written `x"dead_beef"`, pairs of them between `x"` and `"`, as many as there
are, an underscore between any two digits. Single quotes hold one, as they hold one
character, and double quotes any number, as they do of text.

```trendy
const magic := x"89_50_4e_47";

operator flipped{b: byte}: byte [
    b xor x'ff'
];

operator main{} [
    call debug_value{v: magic[1]};
    call debug_value{v: magic[1].value};
    call debug_value{v: length{a: magic}};
    call debug_value{v: magic || x"0d0a"};
    call debug_value{v: magic < x"8a"};
    call debug_value{v: x'f0' and x'3c'};
    call debug_value{v: not x'0f'};
    call debug_value{v: flipped{b: byte{value: 15}}};
    call debug_value{v: to_text{from: x'7F'}};
];
```

- **It is a type like any other,** declared in the prelude as `{ value: integer }` of
  a constraint, and ordered by its number, so `b.value` is its number, and
  `byte{value: n}` the byte of a number, which fails outside its range (R0017).
  Its one component takes a byte of memory, and an `array byte` one for each, as an
  integer a type's constraints keep in a range takes only what that range needs,
  whichever type it is of. Nothing in the language can tell.
- **`and`, `or`, `xor` and `not` of bytes take their bits,** and give a byte, as they
  do of integers, in their operators' forms too, `xor{a, b, c}`, and with the same
  grouping, so `b and x'f0' = x'40'` is an error, with a fix to `(b and x'f0') = x'40'`
  (A0076). A byte has no arithmetic and no shifts, which could leave its range: those
  are of its number, `byte{value: b.value * 2 mod 256}`. An integer isn't a byte
  either, so `b and 0x0f` is an error, with a fix to `b and x'0f'`, as is `127` where a
  byte is expected, to `x'7f'` (A0004).
- **A byte isn't a character,** and `array byte` isn't text: `x'61' = 'a'` is an error
  (A0004), whose fix writes a character of ASCII as its byte, and `||` of bytes and text
  is too. Which bytes a text is depends on an encoding, which nothing here chooses.
- **Literals say which at once.** `x'dead'` is an error, with a fix to `x"dead"`, as
  `x''` is to `x""`, an empty array, and `x'7'` has no fix, since the digit it lacks
  could be any (P0061). `x'7f'` where an array is expected, and `x"7f"` where a byte is,
  are fixed to each other (A0004).
- **Written in lowercase,** whichever case a literal's digits are in, so a value is
  written as `x'7f'` and an array of them `x"deadbeef"`, as `to_text` writes them too.
  The empty one is `x""`. A program's subtype of `byte`, as `type ascii is byte {
  constraint value < 128 };`, changes nothing of this: an `array byte` is written
  `x"61ff"` whichever type each element is most specifically of. An `array ascii` is
  of another type, and written as one, `array [x'61']`.

### Comparisons

`a = b`, `a <> b`, `a < b`, `a <= b`, `a > b` and `a >= b` compare two
values of the same type, and are booleans. Every type has `=` and `<>`.
The built-in types are also ordered:

| Type | Order |
|---|---|
| `boolean` | `false` before `true` |
| `integer` | numerical |
| `approximate` | numerical |
| `character` | by its code points, as a text of it is |
| `text` | by code point, which is also the order of the UTF-8 bytes. There is no collation: `"Z" < "a"`, and `"é" > "z"` |

Two values are equal exactly when they are the same value, as *The Third
Manifesto* requires (RM Prescription 8). Two tuples are equal when every
attribute's values are equal, and two relations when they have the same
tuples, in any order; the values inside are compared the same way. Two
arrays are equal when they have equal elements at each position. Tuples have no
order, so `<`, `<=`, `>` and `>=` don't apply to them (A0022).

An array is ordered if its elements are, element by element, the first that differs
deciding, and one that runs out first coming first: `array [1, 2] < array [1, 3]`, and
`array [1] < array [1, 0]`, as text is ordered. That is so of scalars and arrays of
them, and of a type of the program's own whose order is its stored components, all of
them, ascending; an array of any other type has no order (A0022).

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
    /* How many halvings bring each year below 100. */
    call debug_table{r: people extend { steps := [
        var left := born;
        var count := 0;
        while left >= 100 loop [
            left := left div 2;
            count := count + 1;
        ];
        count
    ] }};
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
  assign a variable that isn't its own, or `call` an operator (A0034), and
  it neither warns nor returns (A0035). It reads what the names around it
  let it read, so a program's variable in a read-only operator's body, or
  an `out` parameter not yet assigned, is what it would be outside.
- **Its statements can decide what runs next:** an `if` statement, a loop,
  a transaction and a `fail` statement are what they are in a body, on its
  own local variables, as above with `steps`. It can choose between values
  with an `if` expression too, whose branches can be value blocks; an `if`
  that runs to the end of a body or block, with no `;`, is its value.
- **It is evaluated when compiling** where its values are known, as any
  expression is, unless it has a statement that decides what runs next,
  which is run when the program runs, as it may not stop. So an initial
  value can't have one, directly or through an operator it calls (A0077).

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
- **`f{a}` is a projection if `f` is a value,** and otherwise a call: names
  alone are its own arguments' values (see [A name alone](#a-name-alone)).
  `(f){a}` is always a projection.
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

### A name alone

Where a name is given a value with `:`, in a call's arguments, a selector's or
a tuple's attributes, a name alone is its own value: `a` is `a: a`, as Rust
writes a struct's fields and JavaScript an object's.

```trendy
operator area{width: integer, height: integer}: integer [
    width * height
];

operator room{width: integer, height: integer} [
    call debug_value{v: area{width, height}};
    call debug_value{v: area{height, width: 2}};
    call debug_value{v: tuple { width, height }};
    call debug_value{v: relation { { width, height } }};
];

operator main{} [
    call room{width: 3, height: 4};
];
```

- **It is by name, not by position.** `area{height, width}` gives each its own
  parameter, so arguments can't be swapped. A name that is no parameter's is an
  error (A0018), which says which parameter it is for, when that is certain: the
  only one left, or the one PostgreSQL's or another language's name for it says.
- **What it names is whatever has that name here:** a parameter, a local
  variable, or an attribute of the tuple at hand, as in `r extend { a := area{width,
  height} }`. A name no value has is an error (A0015).
- **A `var` or `out` parameter's is still marked:** `call read{path, out lines};`,
  and the fix adds the mark (A0052).
- **Only where a value goes.** A heading's name and a parameter's take their
  types, and `:=` defines something new, so `extend { a }` means nothing.
- **Writing it out stays valid,** and the formatter leaves both as they are.
- **`f{a, b}` with names alone is a call if `f` is an operator, and a
  projection if `f` is a value.** A value named `f` comes first, so
  `count{r}` calls `count`, unless the program has a variable `count`. If that
  value can't be projected so, the fix calls the operator, if it has exactly
  those parameters. If it can, and each name has a value too, nothing says
  which is meant, which is an error (A0079): `f{a: a, b: b}` is the call, and
  `(f){a, b}` is the projection.

```trendy,error
var totals := relation { { year: 2024, region: "Lima" } };

operator totals{year: integer, region: text}: integer [ year ];

operator show{year: integer, region: text} [
    call debug_value{v: totals{year, region}};
];

operator main{} [];
```

### Names and values: `:`, `:=` and `as`

Three separators pair a name with something, each with one meaning:

- **`name: value`** pairs a name with what is given: a tuple's attribute
  with its value, a heading's with its type, a parameter with its
  argument. Where it gives a value, a name alone is its own (see
  [A name alone](#a-name-alone)).
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
- **An attribute compared with itself is an error** (A0081): `where n = n` is true of
  every tuple. When `n` is a parameter's name too, the attribute hides that only in the
  `where`, so `const wanted := n;` before it reads the parameter, which keeps its name,
  and `where n = wanted` compares them.
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
- **A condition that says what the first attributes equal finds its tuples,**
  by a binary search, rather than going through them all, as a relation's tuples
  are in the order of their attributes' names: `r where k = x` for `k` first,
  and `r where k = x and l = y` for `l` next. The values can be any expression
  that doesn't read the tuple, so `r where k = i - 1` and `r where k =
  f{n: i}` find them too, computing the value once, if there is a tuple, as it
  would fail for the first if it fails. If they can fail and something else is
  left of the condition, the tuples are gone through, as a part that can't fail
  may keep the value from being computed at all, and one that can may fail for a
  tuple the key doesn't find.
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
- **Common attributes must be of one type** in both operands (A0027),
  since values never convert to another type implicitly, or, in a
  hierarchy, of types one of which is a subtype of the other, which the
  result's is (see [Union types](#union-types)).
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
  common attributes: at least one (A0028), of one type, or of types one
  of which is a subtype of the other (A0027). They
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
relations must have the same attributes, so an empty `relation {}` takes
the other's heading, whichever side it is on. They are Tutorial D's `UNION`,
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

- **The same attributes,** each of one type in both (A0031), or of types
  in one hierarchy, as [Union types](#union-types) describes. When each
  operand has one attribute the other lacks, of one type, the fix renames
  it. They take relations; `union` of tuples, Tutorial D's tuple
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

### Transitive closure

`r tclose` is the transitive closure of a relation of two attributes of
one type, each tuple an arc from one value to another: the relation of
the same heading with a tuple for every path, `{ from: x, to: y }` for
each `y` that arcs lead to from `x`. It is Tutorial D's `TCLOSE` (TTM,
chapter 6, with RM Prescription 19), and what SQL's `WITH RECURSIVE`
computes for a graph.

```trendy
var flows := relation {
    { from: "Kagera", to: "Lake Victoria" },
    { from: "Lake Victoria", to: "White Nile" },
    { from: "White Nile", to: "Nile" },
    { from: "Blue Nile", to: "Nile" },
};
var downstream := flows tclose;
var into_the_nile := (flows tclose where to = "Nile"){from};
var stops := relation {
    { from: "Tokyo", to: "Shinagawa" },
    { from: "Shinagawa", to: "Shibuya" },
    { from: "Shibuya", to: "Tokyo" },
    { from: "Shibuya", to: "Meguro" },
};
var on_the_loop := (stops tclose where from = to){from};

operator main{} [
    call debug_table{r: on_the_loop};
];
```

- **Two attributes of one type,** the ends of each arc (A0069). Which
  attribute an arc starts at doesn't matter: closing the arcs the other
  way round gives the same tuples, so neither is named. A relation of
  more attributes is projected on the two first, with an end of another
  type converted by `extend`, and the help names the pairs of one type it
  has.
- **A cycle is a path from a value back to itself,** so
  `r tclose where a = b` is the values on a cycle, as `on_the_loop` is,
  without Meguro, which nothing leads back to.
- **A restriction to one value follows only its paths:** `r tclose where
  from = x` finds what paths from `x` reach, and `r tclose where to = y` what
  reaches `y`, without making the rest of the closure, so a graph whose whole
  closure wouldn't fit in memory can be asked about one value at a time. The value
  is a value that is the same for each tuple, and the result what the closure's
  would be.
- **So does asking about a relation of values:** `(r tclose) matching s` and
  `join{r tclose, s}`, where `s` has an attribute of the closure's, follow only the
  paths from `s`'s values, or to them, and the result is what the whole closure's
  would be. A dense graph, where nearly everything can be reached, takes up to about
  three times as long as making the closure would, and any other far less.
- **Postfix, binding like a projection:** `r tclose where a = b` closes
  `r`, then restricts the closure, and `r{a, b} tclose` closes a
  projection. Tutorial D's `TCLOSE r` is fixed to `r tclose` (P0054).
- **One of Algebra A's operators,** with `remove`, `rename` and the
  joins' AND, and the one the others can't express (TTM Appendix A). Its
  generalized form, which concatenates and aggregates along each path (RM
  Very Strong Suggestion 6), is still to come.

```trendy,error
var flows := relation { { from: "Kagera", to: "Lake Victoria" } };
var downstream := TCLOSE flows;

operator main{} [];
```

### Precedence

From tightest to loosest:

| Operators | Grouping |
|---|---|
| `!!`, of a name or a parenthesized expression | none: see below |
| postfix `.a`, indexing `[…]`, projection `{…}`, `remove {…}`, `rename {…}`, `extend {…}`, `replace {…}`, `group {…}`, `ungroup {…}`, `wrap {…}`, `unwrap {…}` and `tclose` | left |
| prefix `not`, `-`, `+`, `tuple from` and `value from` | |
| `*`, `/`, `div`, `mod`, `shl`, `shr` | left |
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
comparison, parenthesize it: `not (n = 0)`. Writing `not t = "x"` for a
text `t` is an error, since `not` takes a boolean or an integer, and its
fix is that parenthesization; SQL and Python read it that way. Of an
integer `n`, `not n = 0` is an error with no fix, since `(not n) = 0`,
its complement compared, could be meant too (see [Bits](#bits)).
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

The symbols other languages use, such as `&&`, `&`, `|`, `!`, `~`, `<<`,
`>>`, `->`, `%`, `==` and `!=`, are not operators in trendy. Neither are
capitalized spellings such as `AND` or `MOD`. The compiler fixes each to
trendy's operator, of booleans and of integers alike, as `&` and `&&` are
both `and`. `^` is the exclusive or in C, but a power in SQL and R, so
the help says to write `xor`, without a fix, and `>>>`, JavaScript's
unsigned shift, has no fix either, since no integer is unsigned (see
[Bits](#bits)). `||` is concatenation, as in Tutorial D and SQL, so
`a || b` on booleans is an error with a fix to `or`.

```trendy,error
var a := true && false;
var b := 7 % 3;
var c := a == b;

operator main{} [];
```

### Evaluation

Expressions have no side effects. What is computed for each tuple of a relation and
fails for several fails as it does for the first of them in canonical order, the order
`debug_value` writes a relation's tuples in, so that the failure, and the error a transaction collects, depends only
on the relation's value, which has no order of its own (TTM RM Proscription 2), not on
how its tuples happen to be kept. Every operand is evaluated, so an error in
one is never skipped, but for an `if`'s branches, only the one its
condition takes, and for the operands of `and`, `or` and `implies`, each
computed only until one decides the result (see [Boolean
operators](#boolean-operators)), and for a constant's expression, evaluated
when a read first needs it (see [Constants](#constants)). Whatever doesn't depend on parameters,
variables or input is evaluated when compiling, and so is whatever a constant operand decides: `p
and false` is just `false`, whatever `p` is, `true and p` is just `p`, and
`n * 1` is just `n`. But `(n + 1) * 0` is still computed, since `n + 1` can
overflow. A failure in an operand that another, known when compiling,
decides the result without isn't reported, as one in an `if`'s branch not
taken isn't; one known to fail beside others known only when the program
runs is an error, as it fails whenever none of them decides the result. Approximate numbers are simplified the same way, since
they are finite and have one zero: `x + 0e0` is `x`, and `x * 0e0` is `0e0`.

## Failures

A program fails on purpose with `fail e`, where `e` is an *error*: a value
of one of the subtypes of the prelude's union type `error` (see [Union
types](#union-types)), which says what went wrong. Unless a transaction
takes it (see [Transactions](#transactions)), it stops the program,
writing the error's message and where it is, as a failure of the
language's own does: with the code of its kind, R0021 for the prelude's
`failure` and a program's own, or a language's failure's, as R0002 for
`fail division_by_zero{operation: "1 div 0"}`. The prelude's `message{error: error}: text`
writes the message: it has no body, so each subtype has its version. The
prelude's `failure` says what went wrong in its own words, and a program
declares subtypes of its own, each with its `message`, whose components
say what went wrong as values:

```trendy
type no_conversion is error { { unit: text } };

operator message{error: no_conversion}: text [
    "no conversion from " || error.unit || " to grams"
];

operator grams{unit: text}: approximate [
    if unit = "jin" then 500e0 else fail no_conversion{unit: unit}
];

operator weigh{unit: text} [
    if unit = "" then [
        fail failure{message: "no unit"};
    ];
    call debug_value{v: grams{unit: unit}};
];

operator main{} [
    call weigh{unit: "jin"};
];
```

- **`fail e` is an expression, and `fail e;` a statement.** A `fail` has
  no value, as the program stops there, so its type is the one expected
  where it is: a declared type's, an operator's result type, or the other
  branch's of an `if`, as an empty relation's heading is (A0011). Its error
  is all that follows `fail`, so as an operand it needs parentheses
  (P0022). `fail` is a contextual keyword, a name elsewhere.
- **A read-only operator can fail,** as `div` does for a divisor of zero:
  it is still a function of its arguments, a partial one, which fails for
  the same arguments whenever it is called. It fails in its value, as in a
  branch of an `if`, or with a `fail` statement, which is then computed
  when the program runs, as any such statement is.
- **Its message is written with its control characters and
  bidirectional controls escaped,** as a table's cell is, so that a
  message made from a file's text can't change the terminal or show what
  it doesn't say, but with its backslashes as they are: the values a
  message shows, as `to_text` writes them, have escapes of their own, as
  in `"4\t2" isn't an integer`.
- **A `fail` fails when it is computed,** and a branch not taken isn't.
  One the compiler computes is reported when compiling, as other failures
  are: an initial value's, or a call's with arguments known then (R0021).
- **An error is a value of one of `error`'s subtypes** (A0004): a text is
  the message of the prelude's `failure`, `fail failure{message: "…"}`,
  which is fixed. Java's, JavaScript's and C++'s `throw` and Python's and
  Ruby's `raise`, and PL/pgSQL's `RAISE EXCEPTION`, are fixed to `fail`
  (P0047). A `fail` always gives its
  error: `fail;` alone, as another language rethrows the exception it
  handles, is reported (P0047), as nothing is ever being handled.
- **The prelude's errors of the language's own failures** have plain
  components, a value of any type written as text, as `debug_value`
  writes it: `overflow { operation }` (R0001), `division_by_zero {
  operation }` (R0002), `tuple_mismatch { attribute, first, second }`
  (R0004), `not_one_tuple { count }` (R0005), `shared_tuple { value }`
  (R0006), `missing_tuple { value }` (R0007), `key_violated { variable,
  key, first, second }` and `component_key_violated { type, component,
  key, first, second }` (R0008), `tuple_removed_twice { value }` and
  `element_set_twice { element, within }` (R0009), `no_min_or_max {
  aggregate }` (R0010), `index_out_of_range { position, length }`
  (R0011), `unparsable { text, type, out_of_range }` (R0014),
  `broken_constraint { value, type, constraint }` (R0017),
  `no_square_root { of }` (R0018), `not_of_type { value, type }` (R0019),
  `overlapping_subtypes { value, first, second }` (R0020), and
  `constraint_violated { constraint, condition }` and
  `constraint_violated_by { constraint, condition, tuples }` (R0023), each with
  the message the failure writes: the failure fails with its error, which
  a transaction takes, as it takes a `fail`'s, and the program writes
  with its message and code if nothing does. A built-in that reads input gives the world's errors as
  values instead, when its call names `out errors`, which `trendy.files`
  declares with it (see [Reading and writing files](#reading-and-writing-files)): `unreadable_file {
  path, reason }` (R0012) and `not_utf8 { path, line }` (R0013), from
  `load_text_file` and `load_csv`, and from `load_csv` `malformed_csv {
  path, line, problem }` (R0015), `missing_column { path, attribute,
  column, columns }` (R0016), `unreadable_field { path, line, column,
  attribute, type, text, out_of_range }` (R0014),
  `broken_constraint_in_file { path, line, attribute, value, type,
  constraint }` (R0017), `component_key_violated_in_file { path, line,
  attribute, type, component, key, first, second }` (R0008) and
  `overlapping_subtypes_in_file { path, line, attribute, value, first,
  second }` (R0020), and from `save_text_file` `unwritable_file { path,
  reason }` (R0026). Without `out errors`, it fails with them, and the
  program writes them as it writes any failure's error. `not_one_line {
  path, line }` (R0027), a mistake in a call of `save_text_file`, it fails
  with either way.

## Transactions

A transaction is a block of statements that are all done or none are
(TTM OO Prescription 4): `transaction [ … ];`. If a statement in it
fails, the transaction undoes them all, each variable they assigned
given back the value it had as the transaction began, and the failure
goes on, to the transaction around it, or else the program's end. With
`collecting e`, the transaction takes the failure instead: it adds the
failure's error to `e`, a variable of type `relation
transaction_collection` declared before it, or another unit's, which
can be qualified, as in `collecting lakes::refused`, `failed` true, and the
statements after the transaction run. If none fails, what they did is
kept. Either way, the transaction adds each warning its statements gave,
with `warn`, `failed` false. The prelude declares the heading:
`transaction_collection := { error: error, failed: boolean }`.

```trendy
var granaries: relation { city: text, sacks: integer } := relation {
    { city: "Accra", sacks: 40 },
    { city: "Tashkent", sacks: 10 },
}
key { city };

type too_few is error { { city: text, sacks: integer } };

operator message{error: too_few}: text [
    error.city || " has " || to_text{from: error.sacks} || " sacks"
];

operator transfer{origin: text, destination: text, n: integer} [
    update granaries { sacks := sacks + n } where city = destination;
    var have := sum{r: (granaries where city = origin){sacks}, of: sacks};
    if have < n then [
        fail too_few{city: origin, sacks: have};
    ];
    update granaries { sacks := sacks - n } where city = origin;
];

operator main{} [
    var found: relation transaction_collection;
    transaction collecting found [
        call transfer{origin: "Tashkent", destination: "Accra", n: 15};
    ];
    call debug_value{v: (found extend { message := message{error: error} }){message}};
    call debug_table{r: granaries};
];
```

Here Accra gets its 15 sacks before Tashkent turns out to have only 10,
so the transaction undoes the transfer, Accra's sacks too, and `found`
holds the error: `granaries` is as it was.

- **A failure doesn't depend on what encloses it.** An operator
  fails, and whatever encloses it decides what that means, so no
  operator takes a parameter for its errors (TTM, chapter 5, rules out
  "a feedback argument on each and every operator invocation").
- **Transactions nest** (TTM OO Prescription 5), with the same syntax at
  every depth, so that code can begin one "without having to know whether
  that transaction will be part of some outer transaction": an inner one's
  failure undoes its own block, and, without
  `collecting`, goes on to the one around it, which undoes its own; with,
  the one around it goes on.
- **A transaction undoes everything its statements assigned:** the
  program's variables, through the operators they call too, at any
  depth, and the local variables of the operator it is in, declared
  outside its block, and the variables passed to that operator's `var`
  and `out` parameters. What it wrote, as with `debug_value`, stays
  written.
- **`collecting` names a variable, and adds to it:** one the operator
  can assign, of type `relation transaction_collection` (A0004),
  declared before the transaction, which the fix for one that isn't does
  (A0015). So one variable collects the errors of a loop's transactions,
  and the statements after them test it as a relation, as in `if not
  is_empty{r: found where failed} then [ … ];`. Adding to an `out`
  parameter reads it, so it is assigned before (A0054). A transaction
  declares no variable: what it collects outlives it, so it is the
  program's to declare, as any variable is.
- **`warn e;` warns with the error `e`, and carries on.** The innermost
  transaction with `collecting` running, whatever operator the warning
  is in, collects it, so no operator takes a parameter for its warnings
  either; a transaction without `collecting` leaves its warnings to the
  one around it. Whether a warning stopped anything is a fact about where
  it happened, not about the error, so any error can be one, and
  `failed` says which an error in a collection was. A transaction's
  warnings survive its rollback, beside its failure, as what happened
  while it ran. With no transaction collecting, the program writes a
  warning's message, and where it warned, as it warns, and carries on
  (R0022). Only an update
  operator warns: a read-only operator, or a value block, computes a
  value, which the compiler may compute before the program runs, when a
  warning would be lost (A0035). `warn` is a contextual keyword, a name
  elsewhere. PL/pgSQL's `RAISE WARNING` and `RAISE NOTICE`, and a call
  of another language's operator that warns, as JavaScript's
  `console.warn(…)` and Python's `warnings.warn(…)`, are fixed to it
  (P0051).
- **An `out` parameter only a collecting transaction assigns** isn't
  assigned on every path, since a failure the transaction takes undoes
  the assignment (A0054); one without `collecting` assigns what its block
  does, as its failure goes on.
- **A transaction's statements read and write nothing,** at any depth
  of the calls they make (A0066): a transaction can be undone, and run
  again, but nothing undoes a read or a write. So a file is read before
  the transaction, and what it gives passed to the statements in it, and
  what a transaction decides to write is kept in a variable it assigns,
  which its rollback undoes too, and written after it. `debug_value` and
  `debug_table` write for the programmer, so they are exempt. The clock is
  read before it too, or the transaction's statements take the instant it began,
  `transaction_start`, which it has before any of them run (A0082), and random
  bits are computed, so `generate` is no read.
- **A transaction is a statement of an operator's body,** read-only too,
  where it undoes what it assigned of its local variables, and takes a
  failure of what it computes, and of a value block, where it does the
  same with the block's own.
- **A failure of the language's own is an error too,** which a
  transaction takes as it takes a `fail`'s: each failure of the
  program's values, a value of the prelude's subtype of `error` for its
  kind (see [Failures](#failures)), as a `division_by_zero`. Only running
  out of memory (R0003) has no error, since making one needs memory too:
  a transaction undoes its statements, and the failure goes on, and
  stops the program. The failures of reading a file can't happen in a
  transaction, which reads nothing (A0066).
- **SQL's habits are reported:** `BEGIN TRANSACTION` and `START
  TRANSACTION` are fixed to `transaction` (P0048); `COMMIT` is removed,
  as a transaction commits where its block ends, and `ROLLBACK` is a
  `fail`, with an error that says why (P0049). Another language's `try`
  and `catch` are a collecting transaction, its errors handled after it
  (P0050).

## Literals

**Boolean literals** are `true` and `false`, in lowercase.

```trendy
var done: boolean := false;
var ready := true;

operator main{} [];
```

**Integer literals** are decimal digits, such as `0` or `42`, up to
`9223372036854775807`, or negative with a `-` before them, down to
`-9223372036854775808`. A radix prefix, `0x`, `0o` or `0b`, in lower
case, writes one in hexadecimal, octal or binary digits, such as `0xFF`,
`0o755` or `0b1010`, a hexadecimal digit after 9 a letter, in either
case. An underscore between two digits separates them, in any radix and
in an approximate literal too, as in `1_000_000`, `0xFFFF_FFFF` or
`6.022_140_76e23`. A decimal number has no leading zero, since C reads
`0755` as octal (P0008).

```trendy
var population := 37_468_000;
var mode := 0o755;
var mask := 0xFF_FF;
var flags := 0b1010_0101;

operator main{} [];
```

```trendy,error
var mode := 0755;

operator main{} [];
```

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

**Byte literals** are two hexadecimal digits between `x'` and `'`, and **bytes
literals**, of an `array byte`, are pairs of them between `x"` and `"`, an underscore
between any two digits (see [Bytes](#bytes)).

```trendy
var low := x'7f';
var signature := x"dead_beef";

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
are case-sensitive. The keywords are `operator`, `var`, `const`, `call`, `builtin`,
`tuple`, `relation`, `array`, `true`, `false`, `not`, `and`, `or`, `xor`,
`implies`, `div`, `mod`, `shl`, `shr`, `remove`, `rename`, `extend`, `replace`, `group`,
`ungroup`, `wrap`, `unwrap`, `tclose`, `join`, `times`, `compose`, `matching`,
`union`, `d_union`, `xunion`, `intersect`, `minus`, `i_minus`, `where`,
`as`, `insert`, `d_insert`, `delete`, `i_delete`, `update`, `assign`,
`return`, `if`, `then`, `else`, `while`, `loop`, `for` and
`transaction`, all lowercase, and a keyword can't be a name (P0017). A
keyword declared as a name is reported once, there, whatever else the word
would then have made of the rest of the file, and the fix renames it wherever it
is a name, leaving what uses it as the keyword as it is.
`type` is a keyword only after `builtin` and before a type's name, `key`
only where a variable's keys go, `value` and `from` only in `tuple from` and `value from`, and
`load`, `from`, `order`, `asc` and `desc` only in a load, `in` only
in a `for` loop, `leave` and `iterate` only before a statement's `;`,
`collecting` only after `transaction`, `heading`
only before a heading's name, `constraint` only before a constraint's
name or condition, and `database` only before a database's name.

Operators, values, and types with headings each have a namespace of
their own. Values are variables and parameters, and a parameter hides a
variable of its name. So a variable can have an operator's name, or a
type's, and which one a name is depends on where it is: in
`call greet{…};` it is an operator, and as a value it is a variable or a
parameter. No two operators can have the same name, nor two variables,
nor two headings, nor a heading and a type, nor two constraints, in one
unit (A0001), and a type's name is its selector's, which no other
operator of its unit can have (A0057). No name refers to a constraint, so constraints have a namespace
of their own. A database is a variable, and shares the variables'
namespace; its relvars and constraints are named within it.

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
| `return x;`, `RETURN x` before the end of a body | `return x;` |
| `{ let x = 1; x * 2 }`, Rust's block, in an expression | `[ var x := 1; x * 2 ]` |
| `while (c) { … }`, `while c do … end`, `WHILE c; … END WHILE` | `while c loop [ … ];` |
| `do { … } while (c);`, `repeat … until c` | `loop [ … ] while c;`, `loop [ … ] while not (c);` |
| `break`, `LEAVE` | `leave;`, of the innermost loop |
| `continue`, `ITERATE` | `iterate;`, of the innermost loop |
| `outer: for (…)`, `break outer;`, Tutorial D's `name: WHILE …` and `LEAVE name` | `for … loop as outer [ … ];`, `leave outer;` |
| `foreach (x in a) { … }`, `for x of a`, `for x in a:` | `for x in a loop [ … ];` |
| `for (i = 0; i < n; i++)` | `var i := 1; while i <= n loop [ …; i := i + 1; ];` |
| `print(x)`, `f(x)` | `call debug_value{v: x};` |
| `x + 1;`, an expression as a statement | `x := x + 1;` to keep its value, `call debug_value{v: x + 1};` to see it |
| `f(x)` in an expression | `f{x: x}`, every argument named |
| `let`, `int x` | `var x`, at the top level or in an operator's body |
| `let mut x` | `var x`: what can change |
| `val x`, Rust's `let x` where nothing assigns it | `const x`: what can't |
| `const int x`, `var int x` | `const x: integer`: a type goes after its name |
| `let x = x + 1;`, declaring a variable again | `x := x + 1;`, assigning it |
| `x = 0` | `x := 0` |
| `x += 1`, `x++`, `++x` | `x := x + 1;` |
| `BLOB`, `bytea`, `VARBINARY`, `bytes`, `byte[]` | `array byte` |
| `u8`, `uint8`, `octet` | `byte` |
| `sys.argv`, `process.argv`, `os.Args`, `std::env::args()`, `String[] args` | `arguments`, of `trendy.process`, without the program's path |
| `argc`, `len(sys.argv)` | `length{a: arguments}` |
| `getenv("HOME")`, `os.environ["HOME"]`, `process.env.HOME`, `$HOME` | `environment where name = "HOME"`, of `trendy.process` |
| `datetime`, `Instant`, `timestamptz` | `timestamp`, of `trendy.time` |
| `datetimeoffset`, `OffsetDateTime` | `offset_timestamp`, of `trendy.time` |
| `interval`, `timedelta`, `TimeSpan` | `duration`, of `trendy.time` |
| `t - s`, of two instants | `elapsed{from: s, to: t}` |
| `t + d`, `t - d`, of an instant and a duration | `after{t, by: d}`, `before{t, by: d}` |
| `d + interval '1 month'` | `add_months{d, months: 1}`, which fails if the month hasn't that day |
| `now()`, `CURRENT_TIMESTAMP`, `Date.now()` | `var at: timestamp; call now{out at};` |
| `now()` in a transaction, `transaction_timestamp()` | `var at: timestamp; call transaction_start{out at};` |
| `'2024-03-05'::date`, `DATE '2024-03-05'` | `date{year: 2024, month: 3, day: 5}` |
| `d + 30`, of a date | `date{days: d.days + 30}` |
| `X'DEADBEEF'`, `'\xdeadbeef'::bytea` | `x"deadbeef"` |
| `b"ab"`, `b'ab'` | `x"6162"` |
| `b'a'`, `0x61u8`, a C `'a'` as a number | `x'61'` |
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
| `string`, `str`, `varchar`, `CHAR`, `CHARACTER` | `text` |
| `char`, `Character` | `character` |
| `'abc'`, `''`, `'''` | `"abc"`, `""`, `'\''` |
| `'a'`, where text is expected | `"a"` |
| `"a"`, where a character is expected | `'a'` |
| `graphemes{t}` | `to_characters{from: t}` |
| `str(x)`, `x.to_string()`, `CAST(x AS VARCHAR)`, `CAST_AS_CHAR (X)` | `to_text{from: x}` |
| `float(n)`, `n as f64`, `CAST(n AS DOUBLE PRECISION)`, `CAST_AS_RATIONAL (N)` | `to_approximate{from: n}` |
| `int(s)`, `parseInt(s)`, `atoi(s)`, `CAST(s AS INTEGER)`, `CAST_AS_INTEGER (S)` | `parse_integer{from: s}` |
| `float(s)`, `parseFloat(s)`, `atof(s)`, `CAST(s AS DOUBLE PRECISION)` | `parse_approximate{from: s}` |
| `Boolean.parseBoolean(s)`, `strconv.ParseBool(s)`, `CAST(s AS BOOLEAN)` | `parse_boolean{from: s}` |
| `lines = readLines(path)`, `File.readlines(path)`, `File.ReadAllLines(path)` | `call load_text_file{path: path, out lines: lines};`, an array of the lines |
| `writeLines(lines, path)`, `f.writelines(lines)`, `File.WriteAllLines(path, lines)`, `fs.writeFile(path, text)` | `call save_text_file{path: path, lines: lines};`, of an array of the lines |
| `rows = pd.read_csv(path, usecols=[0, 2])`, `read_csv(path)` | `call load_csv{path: path, header: true, columns: tuple { city: 1, people: 3 }, out rows: rows};`, an array of tuples whose columns the call names, from 1 |
| `{ x: 1 }`, `TUPLE { X 1 }` | `tuple { x: 1 }` |
| `type Home = { … }`, `interface Home { … }`, `struct Home { … }` | `heading home := { … };`, whose types are `relation home` and `tuple home` |
| `TYPE PLANE_FIGURE UNION;`, an abstract class | `type plane_figure union;`, whose values are its subtypes' |
| `class Ellipse extends PlaneFigure`, `class Ellipse : PlaneFigure`, `TYPE ELLIPSE IS { PLANE_FIGURE POSSREP … }` | `type ellipse is plane_figure { { a: approximate, … } };` |
| `throw e`, `raise e`, `panic!("…")`, `RAISE EXCEPTION '…'` | `fail e`, of an error, as `fail failure{message: "…"}` |
| `unit lakes; interface … implementation … end.`, `module Lakes where` | a file is a unit named by its path, with `interface` and `implementation` alone on their lines |
| `pub fn`, `export function`, `public:` | declared after `interface`, with its body after `implementation` |
| `console.warn(…)`, `warnings.warn(…)`, `RAISE WARNING '…'` | `warn e;`, of an error, which a transaction collects, or the program writes |
| `class NoConversion extends Exception`, an exception of a program's own | `type no_conversion is error { { unit: text } };`, with its `message{error: no_conversion}` |
| `x instanceof Ellipse`, `isinstance(x, Ellipse)`, `x IS OF (ELLIPSE)`, `IS_ELLIPSE(X)` | `x is ellipse`, after which `x` is an ellipse in the rest of the condition |
| `(Ellipse) x`, `x as Ellipse`, `TREAT(x AS ELLIPSE)`, `TREAT_AS_ELLIPSE(X)` | `treat x as ellipse`, or, where it may not be one, `x is ellipse and …` |
| an abstract method, `abstract double area();`, and its overrides | `operator area{f: plane_figure}: approximate;`, and a version for each subtype, `operator area{f: ellipse}: approximate [ … ];` |
| `OPERATOR AREA_OF VERSION AREA_OF_C ( C CIRCLE ) …` | `operator area_of{c: circle} …`: a version has its specification's name |
| `[1, 2]`, `ARRAY[1, 2]` | `array [1, 2]` |
| `[]`, an empty list | `array integer []`, with its elements' type, or `array []` where it is known |
| `a[0]`, the first element | `a[1]` |
| `a[-1]`, the last element | `a[length{a: a}]` |
| `len(a)`, `a.length`, `a.length()`, `CARDINALITY(a)`, `size(a)` | `length{a: a}` |
| `Point { x, y }`, `{ x, y }`, `f(x=x, y=y)` | `point{x, y}`, `tuple { x, y }`, `f{x, y}`: a name alone is its own value |
| `generate_series(1, 9, 2)`, `range(1, 10, 2)` | `series{from: 1, to: 9, step: 2}` |
| `assert n > 0;`, `require(n > 0);`, `CHECK (n > 0)`, Ada's `with Pre => N > 0`, Eiffel's `require` | `operator f{n: integer, constraint n > 0} …`, of what a call is given; `if not (n > 0) then [ fail failure{message: "…"}; ];`, of what a body computes |
| `r.sum(of: price)`, a method call | `sum{r: r, of: price}`: the value is an argument, named as the others are |
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
| `a ^ b`, `a != b` for booleans | `a xor b`; `^` has no fix, as it is a power in SQL and R |
| `a & b`, `a \| b`, `~a`, of integers | `a and b`, `a or b`, `not a` |
| `a << n`, `a >> n` | `a shl n`, `a shr n` |
| `a >>> n`, an unsigned shift | `(a and 0xFFFF_FFFF) shr n`, of a 32-bit word |
| `x & 4 == 4` | `(x and 4) = 4` |
| `0755`, C's octal | `0o755` |
| `0XFF`, `1__000` | `0xFF`, `1_000` |
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
| `TCLOSE R`, SQL's `WITH RECURSIVE` for what a graph reaches | `r tclose` |
| `GROUP BY a` with the group's rows | `r group { rows := {…every attribute but a…} }` |
| `PRIMARY KEY (a)`, `UNIQUE (a)`, `KEY { A }` | `key { a }` |
| `CONSTRAINT c e`, `CREATE ASSERTION c CHECK (e)` | `constraint c := e;` |
| `CREATE DATABASE d` | `database d;` |
| `VAR R REAL RELATION { … }`, `CREATE TABLE d.r (…)` | `var d.r: relation { … };` |
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
| `PI`, `M_PI`, `math.pi`, `Math.PI`, `math::pi` | `pi`, a constant |
| `sine(x)`, `cosine(x)`, `arctan2(y, x)` | `sin{of: x}`, `cos{of: x}`, `atan2{y: y, x: x}` |
| `tan(x)`, `atan(x)`, `hypot(x, y)`, `abs(x)` | `sin{of: x} / cos{of: x}`, `atan2{y: x, x: 1e0}`, `sqrt{of: x * x + y * y}`, `if x < 0e0 then -x else x` |
| `inf`, `NaN`, `1e0 / 0e0` | an error: approximate numbers are finite |

The compiler recognizes each of these habits and suggests the fix.
`trendyc --explain <code>` describes any error code in detail, and
`trendyc --error-format=json` prints each error as JSON, with its fixes as
exact edits.
