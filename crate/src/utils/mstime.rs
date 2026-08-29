/// Current wall-clock time packed into a 32-bit MS-DOS-style timestamp:
/// bits 25-31 year (since 1980), 21-24 month, 16-20 day, 11-15 hour,
/// 5-10 minute, 0-4 second. Same shape STFS file-listing entries use for
/// `createdTimeStamp`/`accessTimeStamp`.
/// <https://free60.org/System-Software/Formats/STFS/#file-listing>
///
/// The year field is masked with `& 0xEF` here rather than the `& 0x7F`
/// a standard MS-DOS timestamp would use for a 7-bit year, matching what
/// STFS packages actually contain.
pub(crate) fn ms_timestamp_now() -> u32 {
    let now = js_sys::Date::new_0();
    let year = now.get_full_year();
    let month = now.get_month() + 1; // js_sys::Date months are 0-indexed
    let month_day = now.get_date();
    let hours = now.get_hours();
    let minutes = now.get_minutes();
    let seconds = now.get_seconds();
    let year_bits = year.wrapping_sub(1980) & 0xEF;
    (year_bits << 25)
        | ((month & 0xF) << 21)
        | ((month_day & 0x1F) << 16)
        | ((hours & 0x1F) << 11)
        | ((minutes & 0x3F) << 5)
        | (seconds & 0x1F)
}
