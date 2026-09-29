//! Conversion rows: physical units, number bases and live currency —
//! typed straight into the search box next to the calculator.
//!
//! Syntax: `<number> <unit> (to|in|=|->) <unit>`, or just `<number>
//! <unit>` for a list of equivalents (`10 km`). Word aliases work
//! (`10 miles to km`), the number may touch its unit (`10km to m`), and
//! each dimension has its own base unit, so mismatched targets
//! (`10 km to lb`) are quietly ignored instead of answered wrongly.
//!
//! Number bases: `255 to hex`, `0xff to dec`, `0b1010 to oct`, bare
//! `0xff`. Money: `100 usd to eur`, symbols (`100 $ to €`) and names
//! (`100 euros in pounds`) — rates come from [`crate::search::currency`].
//!
//! Everything formats through the calculator settings (precision,
//! separators, scientific notation) and Enter copies `value unit` — the
//! same copy/paste behavior as a calculator result.

use crate::config::Config;
use crate::search::calculator::{enter_label, format_number};
use crate::search::currency::{self, Rates};
use crate::search::{Action, ResultKind, SearchResult};
use std::collections::HashMap;
use std::sync::OnceLock;

const D_LEN: u8 = 0;
const D_MASS: u8 = 1;
const D_TEMP: u8 = 2;
const D_VOL: u8 = 3;
const D_AREA: u8 = 4;
const D_SPEED: u8 = 5;
const D_TIME: u8 = 6;
const D_DATA: u8 = 7;
const D_PRESS: u8 = 8;
const D_ENERGY: u8 = 9;
const D_POWER: u8 = 10;
const D_FORCE: u8 = 11;
const D_ANGLE: u8 = 12;
const D_FREQ: u8 = 13;
const D_FUEL: u8 = 14;
const NDIM: usize = 15;

/// One unit of a dimension. Conversion runs through the dimension's
/// base: `base = v * f + o` (temperature's `o` carries its zero
/// offset), or `base = f / v` for the reciprocal fuel-economy units —
/// which is what lets `mpg → L/100km` invert the way it should.
struct Unit {
    sym: &'static str,
    dim: u8,
    f: f64,
    o: f64,
    recip: bool,
    /// Extra accepted spellings (plural names, word aliases); the
    /// canonical `sym` is always accepted too. Lookup is
    /// case-insensitive (`mm`/`MM` resolve the same — the table itself
    /// never differs by case alone).
    alt: &'static [&'static str],
}

macro_rules! u {
    ($sym:literal, $dim:expr, $f:expr, $o:expr, [$($a:literal),* $(,)?]) => {
        Unit { sym: $sym, dim: $dim, f: $f, o: $o, recip: false, alt: &[$($a),*] }
    };
    (recip $sym:literal, $dim:expr, $f:expr, [$($a:literal),* $(,)?]) => {
        Unit { sym: $sym, dim: $dim, f: $f, o: 0.0, recip: true, alt: &[$($a),*] }
    };
}

static UNITS: &[Unit] = &[
    // Length — base: m
    u!("nm", D_LEN, 1e-9, 0.0, ["nanometer", "nanometers", "nanometre", "nanometres"]),
    u!("µm", D_LEN, 1e-6, 0.0, ["micrometer", "micrometers", "micrometre", "micrometres", "micron", "microns", "um"]),
    u!("mm", D_LEN, 1e-3, 0.0, ["millimeter", "millimeters", "millimetre", "millimetres"]),
    u!("cm", D_LEN, 1e-2, 0.0, ["centimeter", "centimeters", "centimetre", "centimetres"]),
    u!("m", D_LEN, 1.0, 0.0, ["meter", "meters", "metre", "metres"]),
    u!("km", D_LEN, 1e3, 0.0, ["kilometer", "kilometers", "kilometre", "kilometres"]),
    u!("in", D_LEN, 0.0254, 0.0, ["inch", "inches"]),
    u!("ft", D_LEN, 0.3048, 0.0, ["foot", "feet"]),
    u!("yd", D_LEN, 0.9144, 0.0, ["yard", "yards"]),
    u!("mi", D_LEN, 1609.344, 0.0, ["mile", "miles"]),
    u!("nmi", D_LEN, 1852.0, 0.0, ["nauticalmile", "nauticalmiles", "nautical mile", "nautical miles"]),
    u!("au", D_LEN, 149_597_870_700.0, 0.0, ["astronomicalunit", "astronomicalunits"]),
    u!("ly", D_LEN, 9.460_730_472_580_8e15, 0.0, ["lightyear", "lightyears", "light year", "light years"]),
    u!("pc", D_LEN, 3.085_677_581_491_367_3e16, 0.0, ["parsec", "parsecs"]),
    // Mass — base: kg
    u!("mg", D_MASS, 1e-6, 0.0, ["milligram", "milligrams"]),
    u!("g", D_MASS, 1e-3, 0.0, ["gram", "grams", "gramme", "grammes"]),
    u!("kg", D_MASS, 1.0, 0.0, ["kilo", "kilos", "kilogram", "kilograms", "kilogramme", "kilogrammes"]),
    u!("t", D_MASS, 1e3, 0.0, ["tonne", "tonnes"]),
    // "ton"/"tons" mean the US short ton (like most converters);
    // metric tonnes are "t"/"tonne(s)".
    u!("ton", D_MASS, 907.184_74, 0.0, ["tons", "short ton", "short tons", "us ton", "us tons"]),
    u!("oz", D_MASS, 0.028_349_523_125, 0.0, ["ounce", "ounces"]),
    u!("lb", D_MASS, 0.453_592_37, 0.0, ["pound", "pounds", "lbs"]),
    u!("st", D_MASS, 6.350_293_18, 0.0, ["stone", "stones"]),
    // Temperature — base: °C (affine: offsets, not factors, move the zero)
    u!("°C", D_TEMP, 1.0, 0.0, ["c", "celsius", "centigrade"]),
    u!("°F", D_TEMP, 5.0 / 9.0, -160.0 / 9.0, ["f", "fahrenheit"]),
    u!("K", D_TEMP, 1.0, -273.15, ["k", "kelvin", "kelvins", "°k"]),
    // Volume — base: L
    u!("mL", D_VOL, 1e-3, 0.0, ["ml", "milliliter", "milliliters", "millilitre", "millilitres", "cc"]),
    u!("L", D_VOL, 1.0, 0.0, ["l", "liter", "liters", "litre", "litres"]),
    u!("cm³", D_VOL, 1e-3, 0.0, ["cm3", "cm^3", "cubic centimeter", "cubic centimeters", "cubic centimetre", "cubic centimetres"]),
    u!("m³", D_VOL, 1e3, 0.0, ["m3", "m^3", "cubic meter", "cubic meters", "cubic metre", "cubic metres"]),
    u!("tsp", D_VOL, 0.004_928_921_593_75, 0.0, ["teaspoon", "teaspoons"]),
    u!("tbsp", D_VOL, 0.014_786_764_781_25, 0.0, ["tablespoon", "tablespoons"]),
    u!("fl oz", D_VOL, 0.029_573_529_562_5, 0.0, ["floz", "fluid ounce", "fluid ounces"]),
    u!("cup", D_VOL, 0.236_588_236_5, 0.0, ["cups"]),
    u!("pt", D_VOL, 0.473_176_473, 0.0, ["pint", "pints", "us pint", "us pints"]),
    u!("qt", D_VOL, 0.946_352_946, 0.0, ["quart", "quarts"]),
    u!("gal", D_VOL, 3.785_411_784, 0.0, ["gallon", "gallons", "us gallon", "us gallons"]),
    u!("imp gal", D_VOL, 4.546_09, 0.0, ["impgal", "imperial gallon", "imperial gallons", "uk gal", "uk gallon", "uk gallons"]),
    u!("imp pt", D_VOL, 0.568_261_25, 0.0, ["imppt", "imperial pint", "imperial pints", "uk pint", "uk pints"]),
    // Area — base: m²
    u!("mm²", D_AREA, 1e-6, 0.0, ["mm2", "mm^2"]),
    u!("cm²", D_AREA, 1e-4, 0.0, ["cm2", "cm^2"]),
    u!("m²", D_AREA, 1.0, 0.0, ["m2", "m^2", "square meter", "square meters", "square metre", "square metres", "sqm", "sq m"]),
    u!("km²", D_AREA, 1e6, 0.0, ["km2", "km^2", "square kilometer", "square kilometers", "square kilometre", "square kilometres", "sqkm", "sq km"]),
    u!("ha", D_AREA, 1e4, 0.0, ["hectare", "hectares"]),
    u!("in²", D_AREA, 0.000_645_16, 0.0, ["in2", "in^2", "square inch", "square inches", "sqin", "sq in"]),
    u!("ft²", D_AREA, 0.092_903_04, 0.0, ["ft2", "ft^2", "square foot", "square feet", "sqft", "sq ft"]),
    u!("yd²", D_AREA, 0.836_127_36, 0.0, ["yd2", "yd^2", "square yard", "square yards"]),
    u!("acre", D_AREA, 4046.856_422_4, 0.0, ["acres"]),
    u!("mi²", D_AREA, 2_589_988.110_336, 0.0, ["mi2", "mi^2", "square mile", "square miles", "sqmi", "sq mi"]),
    // Speed — base: m/s
    u!("m/s", D_SPEED, 1.0, 0.0, ["mps", "meter per second", "meters per second", "metre per second", "metres per second"]),
    u!("km/h", D_SPEED, 1.0 / 3.6, 0.0, ["kmh", "kph", "km/hr", "kilometers per hour", "kilometres per hour"]),
    u!("mph", D_SPEED, 0.447_04, 0.0, ["miles per hour", "mile per hour", "mi/h"]),
    u!("kn", D_SPEED, 1852.0 / 3600.0, 0.0, ["knot", "knots", "kt"]),
    u!("ft/s", D_SPEED, 0.3048, 0.0, ["fps", "feet per second", "foot per second"]),
    // Time — base: s (month/year = average Gregorian)
    u!("ns", D_TIME, 1e-9, 0.0, ["nanosecond", "nanoseconds"]),
    u!("µs", D_TIME, 1e-6, 0.0, ["microsecond", "microseconds", "us"]),
    u!("ms", D_TIME, 1e-3, 0.0, ["millisecond", "milliseconds"]),
    u!("s", D_TIME, 1.0, 0.0, ["sec", "secs", "second", "seconds"]),
    u!("min", D_TIME, 60.0, 0.0, ["mins", "minute", "minutes"]),
    u!("h", D_TIME, 3600.0, 0.0, ["hr", "hrs", "hour", "hours"]),
    u!("day", D_TIME, 86_400.0, 0.0, ["days"]),
    u!("week", D_TIME, 604_800.0, 0.0, ["weeks", "wk", "wks"]),
    u!("month", D_TIME, 2_629_800.0, 0.0, ["months"]),
    u!("year", D_TIME, 31_557_600.0, 0.0, ["years", "yr", "yrs"]),
    // Data — base: byte (decimal SI prefixes, binary IEC suffixes;
    // "kb" is a kilobyte, "bit(s)" is the only way to ask for bits)
    u!("bit", D_DATA, 0.125, 0.0, ["bits"]),
    u!("B", D_DATA, 1.0, 0.0, ["byte", "bytes", "b"]),
    u!("KB", D_DATA, 1e3, 0.0, ["kb", "kilobyte", "kilobytes"]),
    u!("MB", D_DATA, 1e6, 0.0, ["mb", "megabyte", "megabytes"]),
    u!("GB", D_DATA, 1e9, 0.0, ["gb", "gigabyte", "gigabytes"]),
    u!("TB", D_DATA, 1e12, 0.0, ["tb", "terabyte", "terabytes"]),
    u!("PB", D_DATA, 1e15, 0.0, ["pb", "petabyte", "petabytes"]),
    u!("KiB", D_DATA, 1024.0, 0.0, ["kib", "kibibyte", "kibibytes"]),
    u!("MiB", D_DATA, 1_048_576.0, 0.0, ["mib", "mebibyte", "mebibytes"]),
    u!("GiB", D_DATA, 1_073_741_824.0, 0.0, ["gib", "gibibyte", "gibibytes"]),
    u!("TiB", D_DATA, 1_099_511_627_776.0, 0.0, ["tib", "tebibyte", "tebibytes"]),
    // Pressure — base: Pa
    u!("Pa", D_PRESS, 1.0, 0.0, ["pascal", "pascals"]),
    u!("kPa", D_PRESS, 1e3, 0.0, ["kilopascal", "kilopascals"]),
    u!("MPa", D_PRESS, 1e6, 0.0, ["megapascal", "megapascals"]),
    u!("bar", D_PRESS, 1e5, 0.0, ["bars"]),
    u!("mbar", D_PRESS, 100.0, 0.0, ["millibar", "millibars"]),
    u!("atm", D_PRESS, 101_325.0, 0.0, ["atmosphere", "atmospheres"]),
    u!("psi", D_PRESS, 6894.757_293_168_361, 0.0, ["pounds per square inch", "pound per square inch", "lbf/in2"]),
    u!("mmHg", D_PRESS, 133.322_387_415, 0.0, ["mm hg", "millimeter of mercury", "millimeters of mercury", "millimetre of mercury", "millimetres of mercury"]),
    u!("Torr", D_PRESS, 133.322_368_421_052_63, 0.0, ["torrs"]),
    u!("inHg", D_PRESS, 3386.388_640_341, 0.0, ["in hg", "inch of mercury", "inches of mercury"]),
    // Energy — base: J
    u!("J", D_ENERGY, 1.0, 0.0, ["joule", "joules"]),
    u!("kJ", D_ENERGY, 1e3, 0.0, ["kilojoule", "kilojoules"]),
    u!("MJ", D_ENERGY, 1e6, 0.0, ["megajoule", "megajoules"]),
    u!("cal", D_ENERGY, 4.184, 0.0, ["calorie", "calories"]),
    u!("kcal", D_ENERGY, 4184.0, 0.0, ["kilocalorie", "kilocalories", "food calorie", "food calories"]),
    u!("Wh", D_ENERGY, 3600.0, 0.0, ["watt hour", "watt hours", "watthour", "watthours", "watt-hour", "watt-hours"]),
    u!("kWh", D_ENERGY, 3.6e6, 0.0, ["kilowatt hour", "kilowatt hours", "kilowatthour", "kilowatthours", "kilowatt-hour", "kilowatt-hours"]),
    u!("MWh", D_ENERGY, 3.6e9, 0.0, ["megawatt hour", "megawatt hours"]),
    u!("eV", D_ENERGY, 1.602_176_634e-19, 0.0, ["electronvolt", "electronvolts"]),
    u!("BTU", D_ENERGY, 1055.055_852_62, 0.0, ["btus", "british thermal unit", "british thermal units"]),
    u!("therm", D_ENERGY, 105_505_585.257_348, 0.0, ["therms"]),
    // Power — base: W
    u!("W", D_POWER, 1.0, 0.0, ["watt", "watts"]),
    u!("kW", D_POWER, 1e3, 0.0, ["kilowatt", "kilowatts"]),
    u!("MW", D_POWER, 1e6, 0.0, ["megawatt", "megawatts"]),
    u!("GW", D_POWER, 1e9, 0.0, ["gigawatt", "gigawatts"]),
    u!("hp", D_POWER, 745.699_871_582_270_2, 0.0, ["horsepower"]),
    u!("PS", D_POWER, 735.498_75, 0.0, ["metric horsepower", "pferdestarke"]),
    // Force — base: N
    u!("N", D_FORCE, 1.0, 0.0, ["newton", "newtons"]),
    u!("kN", D_FORCE, 1e3, 0.0, ["kilonewton", "kilonewtons"]),
    u!("kgf", D_FORCE, 9.806_65, 0.0, ["kilogram-force", "kilogram force", "kilopond", "kp"]),
    u!("lbf", D_FORCE, 4.448_221_615_260_5, 0.0, ["pound-force", "pound force"]),
    u!("dyn", D_FORCE, 1e-5, 0.0, ["dyne", "dynes"]),
    // Angle — base: rad
    u!("°", D_ANGLE, std::f64::consts::PI / 180.0, 0.0, ["degree", "degrees", "deg"]),
    u!("rad", D_ANGLE, 1.0, 0.0, ["radian", "radians"]),
    u!("grad", D_ANGLE, std::f64::consts::PI / 200.0, 0.0, ["gradian", "gradians", "grades"]),
    u!("turn", D_ANGLE, 2.0 * std::f64::consts::PI, 0.0, ["turns", "rev", "revs", "revolution", "revolutions"]),
    // Frequency — base: Hz
    u!("Hz", D_FREQ, 1.0, 0.0, ["hertz"]),
    u!("kHz", D_FREQ, 1e3, 0.0, ["kilohertz"]),
    u!("MHz", D_FREQ, 1e6, 0.0, ["megahertz"]),
    u!("GHz", D_FREQ, 1e9, 0.0, ["gigahertz"]),
    u!("THz", D_FREQ, 1e12, 0.0, ["terahertz"]),
    u!("rpm", D_FREQ, 1.0 / 60.0, 0.0, ["rev per minute", "revs per minute", "revolutions per minute", "rev/min"]),
    // Fuel economy — base: L/100km; the others are reciprocal (base =
    // f / v), which is the whole point: more mpg = less L/100km.
    u!("L/100km", D_FUEL, 1.0, 0.0, ["l/100 km", "liters per 100 kilometers", "litres per 100 kilometres", "litres per 100km", "liters per 100km"]),
    u!(recip "mpg", D_FUEL, 235.214_583_3, ["mpg (us)", "miles per gallon", "us mpg"]),
    u!(recip "imp mpg", D_FUEL, 282.480_936_3, ["mpg (imp)", "uk mpg", "imperial mpg", "impg", "miles per imperial gallon"]),
    u!(recip "km/L", D_FUEL, 100.0, ["km/l", "km per liter", "kilometers per liter", "kilometres per litre"]),
];

/// The base unit of each dimension (what equivalents lead with).
const BASE_SYM: [&str; NDIM] = [
    "m", "kg", "°C", "L", "m²", "m/s", "s", "B", "Pa", "J", "W", "N", "rad", "Hz", "L/100km",
];

/// Per-dimension, the units the no-target row lists after the primary
/// one — metric first, then the systems people actually think in.
static EQUIV: [&[&str]; NDIM] = [
    &["km", "cm", "mm", "mi", "ft", "in", "yd", "nmi"], // length
    &["g", "t", "lb", "oz", "st", "ton", "mg"],         // mass
    &["°F", "K"],                                        // temperature
    &["mL", "cm³", "gal", "imp gal", "qt", "pt", "cup", "fl oz", "m³"], // volume
    &["ha", "cm²", "m²", "km²", "ft²", "in²", "yd²", "acre", "mi²"], // area
    &["km/h", "mph", "m/s", "kn", "ft/s"],               // speed
    &["min", "h", "day", "week", "s", "ms", "month", "year"], // time
    &["MB", "KB", "GiB", "KiB", "TB", "TiB", "bit"],     // data
    &["kPa", "bar", "psi", "atm", "mmHg", "mbar", "MPa", "Torr"], // pressure
    &["kJ", "cal", "kcal", "Wh", "MJ", "kWh", "BTU"],    // energy
    &["kW", "W", "MW", "hp", "PS"],                      // power
    &["kN", "N", "lbf", "kgf", "dyn"],                   // force
    &["deg", "grad", "turn", "rad"],                     // angle
    &["kHz", "MHz", "GHz", "rpm", "Hz"],                 // frequency
    &["mpg", "imp mpg", "km/L"],                         // fuel economy
];

/// Spelling → unit: two maps built lazily (~130 units, a few hundred
/// keys — once, not per keystroke). Exact matching first so units that
/// differ only by case (`kn` knot vs `kN` kilonewton) stay
/// distinguishable, then a case-folded fallback for convenience
/// (`KNOTS`, `Kib`). Where folding collides, the first table entry wins.
fn index() -> &'static (HashMap<String, &'static Unit>, HashMap<String, &'static Unit>) {
    static IDX: OnceLock<(HashMap<String, &'static Unit>, HashMap<String, &'static Unit>)> =
        OnceLock::new();
    IDX.get_or_init(|| {
        let mut exact: HashMap<String, &'static Unit> = HashMap::new();
        let mut lower: HashMap<String, &'static Unit> = HashMap::new();
        for un in UNITS {
            let keys = std::iter::once(un.sym).chain(un.alt.iter().copied());
            for k in keys {
                exact.entry(k.to_string()).or_insert(un);
                lower.entry(k.to_lowercase()).or_insert(un);
            }
        }
        (exact, lower)
    })
}

fn lookup_unit(token: &str) -> Option<&'static Unit> {
    let t = token.trim();
    if t.is_empty() {
        return None;
    }
    let idx = index();
    idx.0
        .get(t)
        .or_else(|| idx.1.get(&t.to_lowercase()))
        .copied()
}

fn base_unit(dim: u8) -> &'static Unit {
    lookup_unit(BASE_SYM[dim as usize]).expect("every dimension has a base unit")
}

/// Base-unit value of `v` in `u`.
fn to_base(v: f64, u: &Unit) -> f64 {
    if u.recip {
        u.f / v
    } else {
        v * u.f + u.o
    }
}

/// The inverse: base-unit value → `u`.
fn from_base(b: f64, u: &Unit) -> f64 {
    if u.recip {
        u.f / b
    } else {
        (b - u.o) / u.f
    }
}

/// Leading numeric literal ("-12.5", "3") → (value, rest). Anything
/// else (letters first, "1e5") yields None so ordinary searches pass
/// through untouched.
fn leading_number(q: &str) -> Option<(f64, &str)> {
    let b = q.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    let start = i;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    if i == start {
        return None;
    }
    let v: f64 = q[..i].parse().ok()?;
    Some((v, &q[i..]))
}

/// Radix-prefixed integer at the start ("0xff to dec", "-0b1010") →
/// (value, normalized display, rest). "0.5" and friends fall through
/// to [`leading_number`].
fn leading_radix(q: &str) -> Option<(i64, String, &str)> {
    let (neg, body) = match q.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, q),
    };
    let b = body.as_bytes();
    if b.len() < 3 || b[0] != b'0' {
        return None;
    }
    let (radix, prefix) = match b[1] {
        b'x' | b'X' => (16u32, "0x"),
        b'b' | b'B' => (2, "0b"),
        b'o' | b'O' => (8, "0o"),
        _ => return None,
    };
    let mut i = 2;
    while i < b.len() && (b[i] as char).is_digit(radix) {
        i += 1;
    }
    if i == 2 {
        return None;
    }
    let digits = &body[2..i];
    let value = i64::from_str_radix(digits, radix).ok()?;
    let digits = if radix == 16 {
        digits.to_ascii_uppercase()
    } else {
        digits.to_string()
    };
    let display = format!("{}{prefix}{digits}", if neg { "-" } else { "" });
    Some((
        if neg { -value } else { value },
        display,
        &body[i..],
    ))
}

/// First occurrence of `w` as a whole word (start/space before,
/// space/end after) → (byte index, the word itself).
fn find_word<'a>(s: &'a str, w: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(rel) = s[from..].find(w) {
        let i = from + rel;
        let end = i + w.len();
        let before_ok = i == 0 || s[..i].ends_with(' ');
        let after_ok = end >= s.len() || s[end..].starts_with(' ');
        if before_ok && after_ok {
            return Some(i);
        }
        from = i + 1;
    }
    None
}

/// Split the part after the amount into (source, target) at the first
/// separator: `->`, `→`, `to`, `in`, `=`. No separator → the whole
/// string is the source and the target is empty (equivalents).
/// "in" at index 0 is the unit inch, not a separator.
fn split_target(rest: &str) -> (&str, &str) {
    if let Some(i) = rest.find("->") {
        return (rest[..i].trim(), rest[i + 2..].trim());
    }
    if let Some(i) = rest.find('→') {
        return (rest[..i].trim(), rest[i + '→'.len_utf8()..].trim());
    }
    if let Some(i) = find_word(rest, "to") {
        return (rest[..i].trim(), rest[i + 2..].trim());
    }
    if let Some(i) = find_word(rest, "in") {
        if i > 0 {
            return (rest[..i].trim(), rest[i + 2..].trim());
        }
    }
    if let Some(i) = rest.find('=') {
        return (rest[..i].trim(), rest[i + 1..].trim());
    }
    (rest.trim(), "")
}

#[derive(Clone, Copy)]
enum BaseKw {
    Hex,
    Bin,
    Oct,
    Dec,
}

fn base_keyword(t: &str) -> Option<BaseKw> {
    match t.trim().to_lowercase().as_str() {
        "hex" | "hexadecimal" => Some(BaseKw::Hex),
        "bin" | "binary" => Some(BaseKw::Bin),
        "oct" | "octal" => Some(BaseKw::Oct),
        "dec" | "decimal" => Some(BaseKw::Dec),
        _ => None,
    }
}

/// `255 to hex`, `0xff to dec`, `0b1010 to oct`, bare `0xff` —
/// integers only; units never convert into bases (`10 km to hex`).
fn base_row(q: &str, config: &Config) -> Option<SearchResult> {
    if let Some((v, display, rest)) = leading_radix(q) {
        let (from_tok, to_tok) = split_target(rest);
        if !from_tok.is_empty() {
            return None; // trailing junk, not a target
        }
        let kw = if to_tok.is_empty() {
            BaseKw::Dec
        } else {
            base_keyword(to_tok)?
        };
        return Some(base_result(display, v, kw, config));
    }
    let (n, rest) = leading_number(q)?;
    let (from_tok, to_tok) = split_target(rest);
    if !from_tok.is_empty() || to_tok.is_empty() {
        return None;
    }
    let kw = base_keyword(to_tok)?;
    if n.fract() != 0.0 || n.abs() >= 1e15 {
        return None;
    }
    let v = n as i64;
    let display = if n < 0.0 {
        format!("-{}", v.unsigned_abs())
    } else {
        format!("{v}")
    };
    Some(base_result(display, v, kw, config))
}

fn base_result(input: String, v: i64, kw: BaseKw, config: &Config) -> SearchResult {
    let sign = if v < 0 { "-" } else { "" };
    let u = v.unsigned_abs();
    let out = match kw {
        BaseKw::Hex => format!("{sign}0x{u:X}"),
        BaseKw::Bin => format!("{sign}0b{u:b}"),
        BaseKw::Oct => format!("{sign}0o{u:o}"),
        BaseKw::Dec => format!("{sign}{u}"),
    };
    row(
        format!("{input} = {out}"),
        enter_label(config).to_string(),
        out,
    )
}

/// ISO codes recognised without the live table — enough to detect the
/// query (and trigger the first fetch) before any rates have landed.
/// The loaded table extends this to whatever the API publishes.
const ISO: &[&str] = &[
    "usd", "eur", "gbp", "jpy", "cny", "chf", "cad", "aud", "nzd", "inr", "sgd", "hkd", "nok",
    "sek", "dkk", "pln", "czk", "huf", "ron", "try", "rub", "uah", "ils", "thb", "idr", "myr",
    "php", "vnd", "egp", "pkr", "bdt", "ngn", "kes", "zar", "brl", "mxn", "clp", "ars", "cop",
    "pen", "ghs", "tzs", "ugx", "mzn", "all", "mkd", "rsd", "bam", "krw", "xau", "xag", "xpt",
    "xdr", "btc", "eth", "sol", "xrp", "ada", "doge", "ltc", "bnb", "dot", "avax", "xlm",
    "link",
];

/// Symbols and words that mean a currency (→ ISO code).
const CUR_NAME: &[(&str, &str)] = &[
    ("$", "usd"),
    ("us$", "usd"),
    ("dollar", "usd"),
    ("dollars", "usd"),
    ("€", "eur"),
    ("euro", "eur"),
    ("euros", "eur"),
    ("£", "gbp"),
    ("pound", "gbp"),
    ("pounds", "gbp"),
    ("pound sterling", "gbp"),
    ("¥", "jpy"),
    ("yen", "jpy"),
    ("yuan", "cny"),
    ("renminbi", "cny"),
    ("₹", "inr"),
    ("rupee", "inr"),
    ("rupees", "inr"),
    ("₽", "rub"),
    ("ruble", "rub"),
    ("rubles", "rub"),
    ("rouble", "rub"),
    ("roubles", "rub"),
    ("₩", "krw"),
    ("won", "krw"),
    ("₺", "try"),
    ("lira", "try"),
    ("₪", "ils"),
    ("shekel", "ils"),
    ("shekels", "ils"),
    ("₫", "vnd"),
    ("₴", "uah"),
    ("฿", "thb"),
    ("₦", "ngn"),
    ("zł", "pln"),
    ("₿", "btc"),
    ("bitcoin", "btc"),
    ("bitcoins", "btc"),
    ("ethereum", "eth"),
    ("ether", "eth"),
    ("gold", "xau"),
    ("silver", "xag"),
    ("platinum", "xpt"),
    ("franc", "chf"),
    ("francs", "chf"),
    ("peso", "mxn"),
    ("pesos", "mxn"),
    ("real", "brl"),
    ("reais", "brl"),
    ("rand", "zar"),
];

/// A currency intent: static spellings first, then whatever the live
/// rate table carries (300+ codes including crypto and metals).
fn currency_code(token: &str, rates: Option<&Rates>) -> Option<String> {
    let t = token.trim().to_lowercase();
    if t.is_empty() {
        return None;
    }
    if ISO.contains(&t.as_str()) {
        return Some(t);
    }
    if let Some((_, code)) = CUR_NAME.iter().find(|(k, _)| *k == t) {
        return Some((*code).to_string());
    }
    rates.and_then(|r| r.map.contains_key(&t).then_some(t))
}

/// The one shape every conversion row takes.
fn row(title: String, subtitle: String, copy: String) -> SearchResult {
    SearchResult {
        kind: ResultKind::Calculator,
        title,
        subtitle: Some(subtitle),
        icon: Some("accessories-calculator-symbolic".into()),
        action: Action::InsertCalculatorResult(copy),
        score: i32::MAX - 2,
    }
}

/// `10 km to mi` — the direct conversion.
fn unit_row(amount: f64, from: &Unit, to: &Unit, config: &Config) -> Option<SearchResult> {
    let conv = from_base(to_base(amount, from), to);
    if !conv.is_finite() {
        return None;
    }
    let (_, show_from) = format_number(amount, config);
    let (raw, show) = format_number(conv, config);
    Some(row(
        format!("{show_from} {} = {show} {}", from.sym, to.sym),
        enter_label(config).to_string(),
        format!("{raw} {}", to.sym),
    ))
}

/// `10 km` — no target given: lead with a readable primary conversion
/// (a value between 1 and 1e6 wins, else the base unit), then list up
/// to four more in the subtitle.
fn equiv_row(amount: f64, from: &Unit, config: &Config) -> Option<SearchResult> {
    let conv = |u: &Unit| from_base(to_base(amount, from), u);
    let base = base_unit(from.dim);
    let mut cands: Vec<&Unit> = Vec::new();
    if base.sym != from.sym {
        cands.push(base);
    }
    for s in EQUIV[from.dim as usize] {
        if let Some(u) = lookup_unit(s) {
            if u.sym != from.sym && !cands.iter().any(|c| c.sym == u.sym) {
                cands.push(u);
            }
        }
    }
    let readable = |u: &Unit| {
        let v = conv(u);
        v.is_finite() && (1.0..=1e6).contains(&v.abs())
    };
    let prim = cands
        .iter()
        .copied()
        .find(|u| readable(u))
        .or_else(|| {
            if base.sym != from.sym && readable(base) {
                Some(base)
            } else {
                None
            }
        })
        .or_else(|| cands.first().copied())?;
    let prim_val = conv(prim);
    if !prim_val.is_finite() {
        return None;
    }
    let (_, show_from) = format_number(amount, config);
    let (raw_p, show_p) = format_number(prim_val, config);
    let mut sub = enter_label(config).to_string();
    for u in cands.iter().filter(|u| u.sym != prim.sym).take(4) {
        let v = conv(u);
        if !v.is_finite() {
            continue;
        }
        let (_, s) = format_number(v, config);
        sub.push_str(&format!(" · {s} {}", u.sym));
    }
    Some(row(
        format!("{show_from} {} = {show_p} {}", from.sym, prim.sym),
        sub,
        format!("{raw_p} {}", prim.sym),
    ))
}

/// `100 usd to eur` — plus a reverse rate line and the rates' date, so
/// "live" is verifiable right in the row.
fn currency_row(
    amount: f64,
    from: &str,
    to: &str,
    rates: &Rates,
    config: &Config,
) -> Option<SearchResult> {
    let conv = amount * rates.cross(from, to)?;
    if !conv.is_finite() {
        return None;
    }
    let fu = from.to_uppercase();
    let tu = to.to_uppercase();
    let (_, show_from) = format_number(amount, config);
    let (raw, show) = format_number(conv, config);
    let back = rates.cross(to, from)?;
    Some(row(
        format!("{show_from} {fu} = {show} {tu}"),
        format!(
            "{} · 1 {tu} = {} {fu} · {}",
            enter_label(config),
            format_number(back, config).1,
            rates.date
        ),
        format!("{raw} {tu}"),
    ))
}

/// `100 usd` — no target: the majors as equivalents.
fn currency_equiv_row(
    amount: f64,
    from: &str,
    rates: &Rates,
    config: &Config,
) -> Option<SearchResult> {
    let prim = if from == "eur" { "usd" } else { "eur" };
    let mut rest: Vec<&str> = Vec::new();
    for c in ["eur", "usd", "gbp", "jpy", "cny", "inr", "chf", "cad", "aud", "btc"] {
        if c != from && c != prim && rates.map.contains_key(c) {
            rest.push(c);
        }
        if rest.len() == 4 {
            break;
        }
    }
    let conv = amount * rates.cross(from, prim)?;
    if !conv.is_finite() {
        return None;
    }
    let (_, show_from) = format_number(amount, config);
    let (raw_p, show_p) = format_number(conv, config);
    let mut sub = enter_label(config).to_string();
    for c in rest {
        let v = amount * rates.cross(from, c)?;
        let (_, s) = format_number(v, config);
        sub.push_str(&format!(" · {s} {}", c.to_uppercase()));
    }
    sub.push_str(&format!(" · {}", rates.date));
    Some(row(
        format!("{show_from} {} = {show_p} {}", from.to_uppercase(), prim.to_uppercase()),
        sub,
        format!("{raw_p} {}", prim.to_uppercase()),
    ))
}

/// Conversion entry point — called alongside
/// [`crate::search::calculator::evaluate`] for every default-search
/// query. Returns one row or None; unknown words, mismatched
/// dimensions and half-typed queries all pass straight through.
pub fn convert(q: &str, config: &Config) -> Option<SearchResult> {
    if !config.enable_calculator {
        return None;
    }
    let q = q.trim();
    if config.calc_base_convert {
        if let Some(r) = base_row(q, config) {
            return Some(r);
        }
    }
    let (amount, rest) = leading_number(q)?;
    if !amount.is_finite() {
        return None;
    }
    let (from_tok, to_tok) = split_target(rest);
    let from_unit = lookup_unit(from_tok);
    let to_unit = lookup_unit(to_tok);

    if let Some(fu) = from_unit {
        if !config.calc_converter {
            return None;
        }
        if to_tok.is_empty() {
            return if config.calc_equivalents {
                equiv_row(amount, fu, config)
            } else {
                None
            };
        }
        let tu = to_unit?;
        if fu.dim != tu.dim {
            return None;
        }
        return unit_row(amount, fu, tu, config);
    }

    // Not a physical unit — money?
    let rates = currency::cached();
    let fc = currency_code(from_tok, rates.as_deref())?;
    if !config.calc_currency {
        return None;
    }
    // Intent detected: start (or keep) the fetch; until rates land the
    // row appears on the refresh that follows.
    currency::ensure_loaded();
    let table = rates?;
    if to_tok.is_empty() {
        return if config.calc_equivalents {
            currency_equiv_row(amount, &fc, &table, config)
        } else {
            None
        };
    }
    let tc = currency_code(to_tok, Some(table.as_ref()))?;
    currency_row(amount, &fc, &tc, &table, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::calculator;
    use std::time::Instant;

    fn cfg() -> Config {
        Config::default()
    }

    fn rates() -> Rates {
        Rates {
            map: [
                ("usd".to_string(), 1.0),
                ("eur".to_string(), 0.875),
                ("gbp".to_string(), 0.75),
                ("jpy".to_string(), 150.5),
                // Not in the static ISO list: only detectable once the
                // table has loaded.
                ("aed".to_string(), 3.67),
            ]
            .into_iter()
            .collect(),
            date: "2026-09-28".into(),
            at: Instant::now(),
        }
    }

    #[test]
    fn unit_aliases_and_attached_numbers() {
        let c = cfg();
        let r = convert("10 miles to km", &c).expect("alias resolves");
        assert_eq!(r.title, "10 mi = 16.09344 km");
        assert_eq!(
            r.action,
            Action::InsertCalculatorResult("16.09344 km".into())
        );
        let r = convert("10km to m", &c).expect("attached number");
        assert_eq!(r.title, "10 km = 10000 m");
        // Canonical symbols in the title, aliases only as input.
        let r = convert("100 metres in feet", &c).expect("word alias");
        assert_eq!(r.title, "100 m = 328.08399 ft");
    }

    #[test]
    fn temperature_is_affine_not_proportional() {
        let c = cfg();
        assert_eq!(
            convert("32 f to c", &c).unwrap().title,
            "32 °F = 0 °C"
        );
        assert_eq!(
            convert("100 c to f", &c).unwrap().title,
            "100 °C = 212 °F"
        );
        assert_eq!(
            convert("0 k to c", &c).unwrap().title,
            "0 K = -273.15 °C"
        );
        assert_eq!(
            convert("100 °c to °f", &c).unwrap().title,
            "100 °C = 212 °F"
        );
    }

    #[test]
    fn decimal_and_binary_prefixes_stay_distinct() {
        let c = cfg();
        assert_eq!(convert("1 kb to b", &c).unwrap().title, "1 KB = 1000 B");
        assert_eq!(convert("1 kib to b", &c).unwrap().title, "1 KiB = 1024 B");
        assert_eq!(
            convert("1 gigabyte to mib", &c).unwrap().title,
            "1 GB = 953.674316 MiB"
        );
    }

    #[test]
    fn speed_and_inverse_fuel_economy() {
        let c = cfg();
        assert_eq!(
            convert("100 km/h to mph", &c).unwrap().title,
            "100 km/h = 62.137119 mph"
        );
        // Reciprocal units invert: more mpg = fewer litres per 100 km.
        assert_eq!(
            convert("10 l/100km to mpg", &c).unwrap().title,
            "10 L/100km = 23.521458 mpg"
        );
    }

    #[test]
    fn separators_and_precision_flow_through() {
        let mut c = cfg();
        c.calc_separators = true;
        let r = convert("1000000 m to km", &c).unwrap();
        assert_eq!(r.title, "1,000,000 m = 1,000 km");
        assert_eq!(
            r.action,
            Action::InsertCalculatorResult("1000 km".into()),
            "the copied value never gets separators"
        );
    }

    #[test]
    fn scientific_notation_for_astronomical_distances() {
        let c = cfg();
        let r = convert("1 ly to m", &c).unwrap();
        assert_eq!(r.title, "1 ly = 9.46073e15 m");
        let mut off = cfg();
        off.calc_sci_notation = false;
        assert_eq!(
            convert("1 ly to m", &off).unwrap().title,
            "1 ly = 9460730472580800 m"
        );
    }

    #[test]
    fn mismatched_and_unknown_targets_dont_answer() {
        let c = cfg();
        assert!(convert("10 km to lb", &c).is_none(), "different dims");
        assert!(convert("10 km to usd", &c).is_none(), "unit → money");
        assert!(convert("10 zorp to km", &c).is_none(), "unknown word");
        assert!(convert("10 km to hex", &c).is_none(), "unit → base");
        assert!(convert("hello world", &c).is_none(), "no amount");
        assert!(convert("10", &c).is_none(), "amount alone");
        assert!(
            convert("10 km to mi", &Config {
                enable_calculator: false,
                ..Config::default()
            })
            .is_none(),
            "the feature switch gates everything"
        );
    }

    #[test]
    fn no_target_shows_equivalents() {
        let c = cfg();
        let r = convert("10 km", &c).expect("equivalents row");
        assert_eq!(r.title, "10 km = 10000 m");
        assert_eq!(r.action, Action::InsertCalculatorResult("10000 m".into()));
        let sub = r.subtitle.unwrap();
        assert!(sub.starts_with("Enter to copy · "), "got {sub}");
        assert!(sub.contains("6.213712 mi"), "got {sub}");
        assert!(sub.contains("32808.39895 ft"), "got {sub}");

        let mut off = cfg();
        off.calc_equivalents = false;
        assert!(convert("10 km", &off).is_none(), "equivalents switch");
        let mut off = cfg();
        off.calc_converter = false;
        assert!(convert("10 km", &off).is_none(), "converter master switch");
        assert!(
            convert("10 km to mi", &off).is_none(),
            "master switch covers explicit targets too"
        );
    }

    #[test]
    fn temperature_equivalents_pick_fahrenheit_first() {
        let c = cfg();
        let r = convert("100 c", &c).expect("temp equivalents");
        assert_eq!(r.title, "100 °C = 212 °F");
        assert!(r.subtitle.unwrap().contains("373.15 K"));
    }

    #[test]
    fn number_bases_convert_both_ways() {
        let c = cfg();
        let r = convert("255 to hex", &c).unwrap();
        assert_eq!(r.title, "255 = 0xFF");
        assert_eq!(r.action, Action::InsertCalculatorResult("0xFF".into()));
        assert_eq!(convert("0xff to dec", &c).unwrap().title, "0xFF = 255");
        assert_eq!(convert("0b1010 to oct", &c).unwrap().title, "0b1010 = 0o12");
        assert_eq!(convert("0o17 to bin", &c).unwrap().title, "0o17 = 0b1111");
        assert_eq!(convert("0xff", &c).unwrap().title, "0xFF = 255");
        assert_eq!(
            convert("-1 to hex", &c).unwrap().title,
            "-1 = -0x1"
        );
        assert!(
            convert("0.5 to hex", &c).is_none(),
            "fractions have no base representation"
        );
        assert!(
            convert("0b1012 to dec", &c).is_none(),
            "invalid digits aren't silently truncated"
        );

        let mut off = cfg();
        off.calc_base_convert = false;
        assert!(convert("255 to hex", &off).is_none());
        assert!(convert("0xff", &off).is_none());
    }

    #[test]
    fn word_separators_and_the_inch_trick() {
        let c = cfg();
        // "in" as source unit vs. as separator.
        assert_eq!(convert("10 in to cm", &c).unwrap().title, "10 in = 25.4 cm");
        assert_eq!(convert("10 cm in in", &c).unwrap().title, "10 cm = 3.937008 in");
        // Other separators, including the arrow forms.
        assert_eq!(convert("5 kg = lb", &c).unwrap().title, "5 kg = 11.023113 lb");
        assert_eq!(convert("5 kg->lb", &c).unwrap().title, "5 kg = 11.023113 lb");
        assert_eq!(convert("5 kg → lb", &c).unwrap().title, "5 kg = 11.023113 lb");
        // A half-typed target falls back to equivalents instead of flickering.
        assert!(convert("10 km to", &c).unwrap().title.starts_with("10 km ="));
    }

    #[test]
    fn the_calculator_leaves_conversions_alone() {
        let c = cfg();
        // Neither path may steal the other's query.
        assert!(calculator::evaluate("10 km to mi", &c).is_none());
        assert!(calculator::evaluate("10 km/h to mph", &c).is_none());
        assert!(convert("1000*1000", &c).is_none());
        assert!(convert("2^10", &c).is_none());
        assert!(calculator::evaluate("2^10", &c).is_some());
    }

    #[test]
    fn currency_rows_use_the_rate_table() {
        let c = cfg();
        let r = rates();
        let row = currency_row(100.0, "usd", "eur", &r, &c).unwrap();
        assert_eq!(row.title, "100 USD = 87.5 EUR");
        assert_eq!(row.action, Action::InsertCalculatorResult("87.5 EUR".into()));
        let sub = row.subtitle.unwrap();
        assert!(sub.contains("1 EUR = 1.142857 USD"), "reverse line: {sub}");
        assert!(sub.ends_with("· 2026-09-28"), "rates date: {sub}");

        let row = currency_equiv_row(100.0, "usd", &r, &c).unwrap();
        assert_eq!(row.title, "100 USD = 87.5 EUR");
        let sub = row.subtitle.unwrap();
        assert!(sub.contains("75 GBP"), "GBP equivalent: {sub}");
        assert!(sub.contains("15050 JPY"), "JPY equivalent: {sub}");
        assert!(sub.ends_with("· 2026-09-28"));
    }

    #[test]
    fn currency_intents_cover_codes_symbols_and_names() {
        let r = rates();
        for (tok, want) in [
            ("usd", "usd"),
            ("EUR", "eur"),
            ("$", "usd"),
            ("€", "eur"),
            ("euros", "eur"),
            ("pounds", "gbp"),
            ("bitcoin", "btc"),
            ("gold", "xau"),
        ] {
            assert_eq!(
                currency_code(tok, Some(&r)).as_deref(),
                Some(want),
                "token {tok}"
            );
        }
        // Static spellings work before any rates have loaded…
        assert_eq!(currency_code("sek", None).as_deref(), Some("sek"));
        // …and the live table extends them afterwards.
        assert_eq!(currency_code("aed", None).as_deref(), None, "not static");
        assert_eq!(
            currency_code("aed", Some(&r)).as_deref(),
            Some("aed"),
            "live table extends detection"
        );
        assert!(currency_code("zorp", Some(&r)).is_none());
        assert!(currency_code("", Some(&r)).is_none());
        // No rates → intent still detected for fetch, no row yet.
        assert_eq!(currency_code("usd", None).as_deref(), Some("usd"));
    }

    #[test]
    fn currency_switch_gates_the_money_path() {
        let mut off = cfg();
        off.calc_currency = false;
        // Detected via the static table (no rates, no network in tests).
        assert!(convert("100 usd to eur", &off).is_none());
        assert!(convert("100 $ to €", &off).is_none());
    }

    #[test]
    fn table_has_no_conflicting_aliases() {
        // A spelling must never mean two different units — the map
        // keeps one of them and silently changes conversions.
        let mut seen: HashMap<String, &str> = HashMap::new();
        for un in UNITS {
            let keys = std::iter::once(un.sym).chain(un.alt.iter().copied());
            for k in keys {
                if let Some(prev) = seen.insert(k.to_string(), un.sym) {
                    assert_eq!(
                        prev, un.sym,
                        "alias {k:?} means both {prev} and {}",
                        un.sym
                    );
                }
            }
        }
    }

    #[test]
    fn case_folding_never_reshuffles_units() {
        // kn (knot) and kN (kilonewton) differ only by case: exact
        // matching keeps both, and the fold keeps the first-comer.
        assert_eq!(lookup_unit("kn").unwrap().sym, "kn");
        assert_eq!(lookup_unit("kN").unwrap().sym, "kN");
        assert_eq!(lookup_unit("KN").unwrap().sym, "kn");
        assert_eq!(lookup_unit("KNOTS").unwrap().sym, "kn");
        assert_eq!(lookup_unit("KiB").unwrap().sym, "KiB");
        assert!(lookup_unit("").is_none(), "empty token is no unit");
    }

    #[test]
    fn every_dimension_base_and_equiv_entry_resolves() {
        for s in BASE_SYM {
            assert!(lookup_unit(s).is_some(), "base {s:?} missing");
        }
        assert_eq!(EQUIV.len(), NDIM);
        for list in EQUIV {
            for s in list {
                assert!(lookup_unit(s).is_some(), "equiv {s:?} missing");
            }
        }
    }
}
