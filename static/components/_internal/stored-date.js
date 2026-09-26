/**
 * The stored form of a date value, computed in the browser exactly as the
 * server stores it (`db::query::helpers::date`), for every value a date input
 * can hold.
 *
 * A date is stored as a UTC instant, `YYYY-MM-DDTHH:MM:SS.mmmZ`:
 *
 *  - an RFC 3339 value with an offset converts to UTC;
 *  - a date alone (`2026-01-15`) is noon of that day;
 *  - a date and time without an offset (`2026-01-15T09:00`, with or without
 *    seconds) is a UTC time — or, when the field stores a timezone and one is
 *    chosen, a local time in that zone, converted to UTC (the earlier of two
 *    readings when the clock falls back; a time the clock skips keeps the UTC
 *    reading);
 *  - anything else — a time alone, a month, text that is no date — is kept as
 *    it is.
 *
 * The display-condition evaluator decodes a date field's value with it, so a
 * condition sees the same date in the browser as on the server.
 *
 * @module stored-date
 * @stability internal
 */

const MS_PER_HOUR = 3_600_000;

/**
 * @typedef {{ year: number, month: number, day: number,
 *   hour: number, minute: number, second: number, ms: number }} DateTimeParts
 */

/**
 * @param {number} n
 * @param {number} width
 * @returns {string}
 */
function pad(n, width) {
  return String(n).padStart(width, '0');
}

/**
 * Whether `year`-`month`-`day` is a calendar date.
 *
 * @param {number} year
 * @param {number} month 1–12
 * @param {number} day
 * @returns {boolean}
 */
function isCalendarDate(year, month, day) {
  if (month < 1 || month > 12 || day < 1) return false;
  const leap = (year % 4 === 0 && year % 100 !== 0) || year % 400 === 0;
  const lengths = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  return day <= lengths[month - 1];
}

/**
 * The UTC epoch milliseconds of `parts` read as a UTC time. `Date.UTC` maps
 * years 0–99 onto 1900–1999, so the year is set on its own.
 *
 * @param {DateTimeParts} parts
 * @returns {number}
 */
function utcMs({ year, month, day, hour, minute, second, ms }) {
  const date = new Date(0);
  date.setUTCFullYear(year, month - 1, day);
  date.setUTCHours(hour, minute, second, ms);
  return date.getTime();
}

/**
 * An instant in the stored form. A year past 9999 carries its sign, as the
 * server writes it.
 *
 * @param {number} ms
 * @param {boolean} [leap] The instant is a leap second (`:60`).
 * @returns {string}
 */
function formatUtc(ms, leap = false) {
  const d = new Date(ms);
  const year = d.getUTCFullYear();
  const yearText =
    year >= 0 && year <= 9999 ? pad(year, 4) : `${year < 0 ? '-' : '+'}${pad(Math.abs(year), 4)}`;
  const second = leap ? 60 : d.getUTCSeconds();

  return (
    `${yearText}-${pad(d.getUTCMonth() + 1, 2)}-${pad(d.getUTCDate(), 2)}` +
    `T${pad(d.getUTCHours(), 2)}:${pad(d.getUTCMinutes(), 2)}:${pad(second, 2)}` +
    `.${pad(d.getUTCMilliseconds(), 3)}Z`
  );
}

/**
 * A date alone, `YYYY-MM-DD`, or `null`.
 *
 * @param {string} value
 * @returns {DateTimeParts | null}
 */
function parseDate(value) {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!m) return null;
  const [year, month, day] = m.slice(1).map(Number);
  if (!isCalendarDate(year, month, day)) return null;
  return { year, month, day, hour: 12, minute: 0, second: 0, ms: 0 };
}

/**
 * A date and time without an offset, `YYYY-MM-DDTHH:MM` or (with
 * `withSeconds`) `YYYY-MM-DDTHH:MM:SS`, or `null`.
 *
 * @param {string} value
 * @param {boolean} withSeconds
 * @returns {DateTimeParts | null}
 */
function parseNaive(value, withSeconds) {
  const pattern = withSeconds
    ? /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})$/
    : /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/;
  const m = pattern.exec(value);
  if (!m) return null;

  const [year, month, day, hour, minute, second = 0] = m.slice(1).map(Number);
  if (!isCalendarDate(year, month, day) || hour > 23 || minute > 59 || second > 59) return null;
  return { year, month, day, hour, minute, second, ms: 0 };
}

/**
 * An RFC 3339 date-time with an offset — `T`, `t` or a space between date
 * and time, any number of fractional digits, `Z`/`z` or `±HH:MM` (a minus
 * sign may be U+2212), a leap second `:60` — as its UTC instant, or `null`.
 *
 * @param {string} value
 * @returns {{ ms: number, leap: boolean } | null}
 */
function parseRfc3339(value) {
  const m =
    /^(\d{4})-(\d{2})-(\d{2})[Tt ](\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?(?:([Zz])|([+\-\u2212])(\d{2}):([0-5]\d))$/.exec(
      value,
    );
  if (!m) return null;

  const [year, month, day, hour, minute, rawSecond] = m.slice(1, 7).map(Number);
  const leap = rawSecond === 60;
  const second = leap ? 59 : rawSecond;
  if (!isCalendarDate(year, month, day) || hour > 23 || minute > 59 || second > 59) return null;

  const ms = Number((m[7] ?? '').slice(0, 3).padEnd(3, '0'));
  const offsetHours = m[8] ? 0 : Number(m[10]);
  if (offsetHours > 23) return null;

  const sign = m[9] === '+' ? 1 : -1;
  const offset = m[8] ? 0 : sign * (offsetHours * 60 + Number(m[11])) * 60_000;
  const local = utcMs({ year, month, day, hour, minute, second, ms });

  return { ms: local - offset, leap };
}

/**
 * The stored form of a date value without a timezone
 * (`normalize_date_value`).
 *
 * @param {string} value
 * @returns {string}
 */
function storedPlain(value) {
  if (value.length <= 8 && value.includes(':') && !value.includes('T')) return value;
  if (value.length === 7 && value[4] === '-' && !value.includes('T')) return value;

  const rfc = parseRfc3339(value);
  if (rfc) return formatUtc(rfc.ms, rfc.leap);

  const parts =
    (value.length === 10 ? parseDate(value) : null) ??
    (value.length === 16 ? parseNaive(value, false) : null) ??
    (value.length === 19 ? parseNaive(value, true) : null);

  return parts ? formatUtc(utcMs(parts)) : value;
}

/**
 * The offset of `zone` from UTC at the instant `ms`, in milliseconds.
 *
 * @param {Intl.DateTimeFormat} format A formatter for the zone.
 * @param {number} ms
 * @returns {number}
 */
function zoneOffset(format, ms) {
  /** @type {Record<string, number>} */
  const at = {};
  for (const { type, value } of format.formatToParts(new Date(ms))) at[type] = Number(value);

  const wall = utcMs({
    year: at.year,
    month: at.month,
    day: at.day,
    hour: at.hour,
    minute: at.minute,
    second: at.second,
    ms: 0,
  });
  return wall - (ms - (((ms % 1000) + 1000) % 1000));
}

/**
 * The earliest UTC instant whose wall clock in `zone` reads `parts`, or
 * `null` when the clock skips that time (or the zone is unknown).
 *
 * @param {DateTimeParts} parts
 * @param {string} zone An IANA zone name.
 * @returns {number | null}
 */
function localToUtc(parts, zone) {
  // The server knows zones by name only; an offset is no zone to it.
  if (/^[+\-\u2212\d]/.test(zone)) return null;

  /** @type {Intl.DateTimeFormat} */
  let format;
  try {
    format = new Intl.DateTimeFormat('en-US', {
      timeZone: zone,
      hourCycle: 'h23',
      year: 'numeric',
      month: 'numeric',
      day: 'numeric',
      hour: 'numeric',
      minute: 'numeric',
      second: 'numeric',
    });
  } catch {
    return null;
  }

  const wall = utcMs(parts);
  const offsets = new Set(
    [-24, 0, 24].map((hours) => zoneOffset(format, wall + hours * MS_PER_HOUR)),
  );
  const readings = [...offsets]
    .map((offset) => wall - offset)
    .filter((instant) => zoneOffset(format, instant) === wall - instant);

  return readings.length > 0 ? Math.min(...readings) : null;
}

/**
 * The stored form of a date value whose field stores a timezone, with
 * `zone` chosen (`normalize_date_with_timezone`, falling back to the plain
 * form where the server does).
 *
 * @param {string} value
 * @param {string} zone
 * @returns {string}
 */
function storedInZone(value, zone) {
  const trimmed = value.trim();
  const parts =
    (trimmed.length === 10 ? parseDate(trimmed) : null) ??
    parseNaive(trimmed, false) ??
    parseNaive(trimmed, true);

  if (!parts) return storedPlain(value);

  const instant = localToUtc(parts, zone);
  return instant === null ? storedPlain(value) : formatUtc(instant);
}

/**
 * The stored form of a date field's value. `zone` is the timezone chosen
 * beside it when the field stores one (empty or absent otherwise).
 *
 * @param {string} value A non-empty date input value.
 * @param {string} [zone]
 * @returns {string}
 */
export function storedDate(value, zone) {
  return zone ? storedInZone(value, zone) : storedPlain(value);
}
