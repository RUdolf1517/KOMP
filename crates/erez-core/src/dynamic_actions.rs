use crate::normalize::normalize_phrase;
use regex::Regex;
use serde_json::Value;
use std::{collections::HashMap, time::Duration};

pub(crate) fn weather(
    location: &str,
    fallback_location: &str,
    result_slot: &str,
) -> Result<HashMap<String, String>, String> {
    let location = resolved_location(location, fallback_location)
        .ok_or_else(|| "укажите базовый район или город в настройках".to_string())?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|err| err.to_string())?;
    let geocoding: Value = client
        .get("https://geocoding-api.open-meteo.com/v1/search")
        .query(&[
            ("name", location.as_str()),
            ("count", "1"),
            ("language", "ru"),
            ("format", "json"),
        ])
        .send()
        .map_err(|err| format!("геокодер погоды недоступен: {err}"))?
        .error_for_status()
        .map_err(|err| format!("геокодер погоды вернул ошибку: {err}"))?
        .json()
        .map_err(|err| format!("неверный ответ геокодера: {err}"))?;
    let place = geocoding
        .get("results")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or_else(|| format!("место `{location}` не найдено"))?;
    let latitude = place
        .get("latitude")
        .and_then(Value::as_f64)
        .ok_or_else(|| "геокодер не вернул широту".to_string())?;
    let longitude = place
        .get("longitude")
        .and_then(Value::as_f64)
        .ok_or_else(|| "геокодер не вернул долготу".to_string())?;
    let display_name = place
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&location)
        .to_string();

    let forecast: Value = client
        .get("https://api.open-meteo.com/v1/forecast")
        .query(&[
            ("latitude", latitude.to_string()),
            ("longitude", longitude.to_string()),
            (
                "current",
                "temperature_2m,apparent_temperature,weather_code,wind_speed_10m".into(),
            ),
            ("timezone", "auto".into()),
        ])
        .send()
        .map_err(|err| format!("сервис погоды недоступен: {err}"))?
        .error_for_status()
        .map_err(|err| format!("сервис погоды вернул ошибку: {err}"))?
        .json()
        .map_err(|err| format!("неверный ответ сервиса погоды: {err}"))?;
    let current = forecast
        .get("current")
        .ok_or_else(|| "в ответе нет текущей погоды".to_string())?;
    let temperature = current
        .get("temperature_2m")
        .and_then(Value::as_f64)
        .ok_or_else(|| "в ответе нет температуры".to_string())?;
    let apparent = current
        .get("apparent_temperature")
        .and_then(Value::as_f64)
        .unwrap_or(temperature);
    let wind = current
        .get("wind_speed_10m")
        .and_then(Value::as_f64)
        .unwrap_or_default();
    let code = current
        .get("weather_code")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let description = weather_code_description(code);
    let temperature_spoken = number_to_russian_words(temperature.round());
    let apparent_spoken = number_to_russian_words(apparent.round());
    let mut phrase =
        format!("В {display_name} сейчас {temperature_spoken} градусов, {description}");
    if (apparent - temperature).abs() >= 3.0 {
        phrase.push_str(&format!(", ощущается как {apparent_spoken}"));
    }
    phrase.push('.');

    Ok(HashMap::from([
        (result_slot.to_string(), format_number(temperature)),
        (format!("{result_slot}_phrase"), phrase),
        ("weather_location".into(), display_name),
        ("weather_temperature".into(), format_number(temperature)),
        ("weather_apparent".into(), format_number(apparent)),
        ("weather_wind".into(), format_number(wind)),
        ("weather_description".into(), description.into()),
    ]))
}

fn resolved_location(location: &str, fallback_location: &str) -> Option<String> {
    [location, fallback_location]
        .into_iter()
        .map(str::trim)
        .find(|value| !value.is_empty() && !value.contains("{{"))
        .map(str::to_string)
}

fn weather_code_description(code: i64) -> &'static str {
    match code {
        0 => "ясно",
        1 => "преимущественно ясно",
        2 => "переменная облачность",
        3 => "пасмурно",
        45 | 48 => "туман",
        51..=57 => "морось",
        61..=67 => "дождь",
        71..=77 => "снег",
        80..=82 => "ливень",
        85 | 86 => "снегопад",
        95 => "гроза",
        96 | 99 => "гроза с градом",
        _ => "погодные условия без уточнения",
    }
}

pub(crate) fn convert_currency(
    amount_text: &str,
    from_text: &str,
    to_text: &str,
    result_slot: &str,
    api_url: &str,
) -> Result<HashMap<String, String>, String> {
    let amount = parse_spoken_number(amount_text)
        .ok_or_else(|| format!("не удалось понять сумму `{amount_text}`"))?;
    let (from_code, from_name) =
        currency(from_text).ok_or_else(|| format!("неизвестная исходная валюта `{from_text}`"))?;
    let (to_code, to_name) =
        currency(to_text).ok_or_else(|| format!("неизвестная целевая валюта `{to_text}`"))?;
    let url = api_url
        .replace("{{from_code}}", from_code)
        .replace("{{to_code}}", to_code);
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("currency api_url must start with http:// or https://".into());
    }

    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|err| err.to_string())?
        .get(&url)
        .send()
        .map_err(|err| format!("сервис курсов недоступен: {err}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("сервис курсов вернул HTTP {status}"));
    }
    let json: Value = response
        .json()
        .map_err(|err| format!("неверный ответ сервиса курсов: {err}"))?;
    if json.get("result").and_then(Value::as_str) == Some("error") {
        return Err(format!(
            "сервис курсов вернул ошибку: {}",
            json.get("error-type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ));
    }
    let rate = json
        .get("rates")
        .and_then(|rates| rates.get(to_code))
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("в ответе нет курса {from_code}/{to_code}"))?;
    let result = amount * rate;
    let amount_spoken = number_to_russian_words(amount);
    let result_spoken = number_to_russian_words(result);
    let amount = format_number(amount);
    let result = format_number(result);

    Ok(HashMap::from([
        (result_slot.to_string(), result.clone()),
        (
            format!("{result_slot}_phrase"),
            format!("{amount_spoken} {from_name} — это примерно {result_spoken} {to_name}"),
        ),
        ("amount".into(), amount),
        ("from_code".into(), from_code.into()),
        ("to_code".into(), to_code.into()),
        ("from_name".into(), from_name.into()),
        ("to_name".into(), to_name.into()),
    ]))
}

pub(crate) fn calculate(
    expression: &str,
    result_slot: &str,
) -> Result<HashMap<String, String>, String> {
    let tokens = expression_tokens(expression)?;
    let mut parser = Parser {
        tokens,
        position: 0,
    };
    let value = parser.expression()?;
    if parser.position != parser.tokens.len() {
        return Err("не удалось разобрать выражение целиком".into());
    }
    if !value.is_finite() {
        return Err("результат не является конечным числом".into());
    }
    let result = format_number(value);
    let spoken_result = number_to_russian_words(value);
    Ok(HashMap::from([
        (result_slot.to_string(), result.clone()),
        (format!("{result_slot}_phrase"), spoken_result),
    ]))
}

fn number_to_russian_words(value: f64) -> String {
    if !value.is_finite() || value.abs() >= 1_000_000_000_000.0 {
        return format_number(value);
    }
    let negative = value < 0.0;
    let absolute = value.abs();
    let rounded_hundredths = (absolute * 100.0).round() as u64;
    let integer = rounded_hundredths / 100;
    let fraction = rounded_hundredths % 100;
    let mut words = integer_to_russian_words(integer);

    if fraction != 0 {
        if fraction % 10 == 0 {
            words.push_str(" целых ");
            words.push_str(&integer_to_russian_words(fraction / 10));
            words.push_str(" десятых");
        } else {
            words.push_str(" целых ");
            words.push_str(&integer_to_russian_words(fraction));
            words.push_str(" сотых");
        }
    }
    if negative {
        format!("минус {words}")
    } else {
        words
    }
}

fn integer_to_russian_words(value: u64) -> String {
    if value == 0 {
        return "ноль".into();
    }
    let scales = [
        (
            1_000_000_000_u64,
            "миллиард",
            "миллиарда",
            "миллиардов",
            false,
        ),
        (1_000_000_u64, "миллион", "миллиона", "миллионов", false),
        (1_000_u64, "тысяча", "тысячи", "тысяч", true),
    ];
    let mut remaining = value;
    let mut parts = Vec::new();
    for (scale, one, few, many, feminine) in scales {
        let group = remaining / scale;
        if group > 0 {
            parts.push(triplet_to_russian_words(group as u16, feminine));
            parts.push(russian_plural(group, one, few, many).into());
            remaining %= scale;
        }
    }
    if remaining > 0 {
        parts.push(triplet_to_russian_words(remaining as u16, false));
    }
    parts.join(" ")
}

fn triplet_to_russian_words(value: u16, feminine: bool) -> String {
    let hundreds = [
        "",
        "сто",
        "двести",
        "триста",
        "четыреста",
        "пятьсот",
        "шестьсот",
        "семьсот",
        "восемьсот",
        "девятьсот",
    ];
    let tens = [
        "",
        "",
        "двадцать",
        "тридцать",
        "сорок",
        "пятьдесят",
        "шестьдесят",
        "семьдесят",
        "восемьдесят",
        "девяносто",
    ];
    let teens = [
        "десять",
        "одиннадцать",
        "двенадцать",
        "тринадцать",
        "четырнадцать",
        "пятнадцать",
        "шестнадцать",
        "семнадцать",
        "восемнадцать",
        "девятнадцать",
    ];
    let units_male = [
        "",
        "один",
        "два",
        "три",
        "четыре",
        "пять",
        "шесть",
        "семь",
        "восемь",
        "девять",
    ];
    let units_female = [
        "",
        "одна",
        "две",
        "три",
        "четыре",
        "пять",
        "шесть",
        "семь",
        "восемь",
        "девять",
    ];
    let mut parts = Vec::new();
    let hundred = value / 100;
    let remainder = value % 100;
    if hundred > 0 {
        parts.push(hundreds[hundred as usize]);
    }
    if (10..20).contains(&remainder) {
        parts.push(teens[(remainder - 10) as usize]);
    } else {
        let ten = remainder / 10;
        let unit = remainder % 10;
        if ten > 0 {
            parts.push(tens[ten as usize]);
        }
        if unit > 0 {
            let units = if feminine { &units_female } else { &units_male };
            parts.push(units[unit as usize]);
        }
    }
    parts.join(" ")
}

fn russian_plural<'a>(value: u64, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    let last_hundred = value % 100;
    if (11..=14).contains(&last_hundred) {
        return many;
    }
    match value % 10 {
        1 => one,
        2..=4 => few,
        _ => many,
    }
}

fn currency(value: &str) -> Option<(&'static str, &'static str)> {
    let value = normalize_phrase(value);
    let aliases: &[(&str, &str, &str)] = &[
        (
            "RUB",
            "российских рублей",
            "руб рубль рубля рублей рублях рубли российский рубль",
        ),
        (
            "UAH",
            "украинских гривен",
            "гривна гривны гривен гривне гривнах гривну грн",
        ),
        (
            "USD",
            "долларов США",
            "доллар доллара долларов доллару доллары бакс бакса баксов usd",
        ),
        ("EUR", "евро", "евро eur"),
        (
            "GBP",
            "британских фунтов",
            "фунт фунта фунтов фунты фунтах стерлинг стерлингов gbp",
        ),
        ("KZT", "казахстанских тенге", "тенге kzt"),
        (
            "BYN",
            "белорусских рублей",
            "белорусский белорусских беларуский беларуских byn",
        ),
        ("CNY", "китайских юаней", "юань юаня юаней юани cny"),
        ("TRY", "турецких лир", "лира лиры лир лирах try"),
        ("GEL", "грузинских лари", "лари gel"),
        ("PLN", "польских злотых", "злотый злотых злотые злотого pln"),
        (
            "CHF",
            "швейцарских франков",
            "франк франка франков франки chf",
        ),
        ("JPY", "японских иен", "иена иены иен йена йены йен jpy"),
    ];
    aliases.iter().find_map(|(code, name, words)| {
        words
            .split_whitespace()
            .any(|alias| value == alias || value.contains(alias))
            .then_some((*code, *name))
    })
}

fn parse_spoken_number(value: &str) -> Option<f64> {
    let normalized = value.trim().replace(',', ".");
    if let Ok(number) = normalized.parse::<f64>() {
        return Some(number);
    }
    let normalized = normalize_phrase(value);
    let words: Vec<&str> = normalized.split_whitespace().collect();
    parse_number_words(&words)
}

fn parse_number_words(words: &[&str]) -> Option<f64> {
    let mut total = 0_f64;
    let mut current = 0_f64;
    let mut found = false;
    for word in words {
        let unit = match *word {
            "ноль" | "zero" => 0,
            "один" | "одна" | "одно" | "one" => 1,
            "два" | "две" | "two" => 2,
            "три" | "three" => 3,
            "четыре" | "four" => 4,
            "пять" | "five" => 5,
            "шесть" | "six" => 6,
            "семь" | "seven" => 7,
            "восемь" | "eight" => 8,
            "девять" | "nine" => 9,
            "десять" | "ten" => 10,
            "одиннадцать" | "eleven" => 11,
            "двенадцать" | "twelve" => 12,
            "тринадцать" | "thirteen" => 13,
            "четырнадцать" | "fourteen" => 14,
            "пятнадцать" | "fifteen" => 15,
            "шестнадцать" | "sixteen" => 16,
            "семнадцать" | "seventeen" => 17,
            "восемнадцать" | "eighteen" => 18,
            "девятнадцать" | "nineteen" => 19,
            "двадцать" | "twenty" => 20,
            "тридцать" | "thirty" => 30,
            "сорок" | "forty" => 40,
            "пятьдесят" | "fifty" => 50,
            "шестьдесят" | "sixty" => 60,
            "семьдесят" | "seventy" => 70,
            "восемьдесят" | "eighty" => 80,
            "девяносто" | "ninety" => 90,
            "сто" | "onehundred" => 100,
            "двести" => 200,
            "триста" => 300,
            "четыреста" => 400,
            "пятьсот" => 500,
            "шестьсот" => 600,
            "семьсот" => 700,
            "восемьсот" => 800,
            "девятьсот" => 900,
            "тысяча" | "тысячи" | "тысяч" | "thousand" => {
                total += current.max(1.0) * 1_000.0;
                current = 0.0;
                found = true;
                continue;
            }
            "миллион" | "миллиона" | "миллионов" | "million" => {
                total += current.max(1.0) * 1_000_000.0;
                current = 0.0;
                found = true;
                continue;
            }
            "hundred" => {
                current = current.max(1.0) * 100.0;
                found = true;
                continue;
            }
            _ => return None,
        };
        current += unit as f64;
        found = true;
    }
    found.then_some(total + current)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Token {
    Number(f64),
    Operator(char),
    LeftParen,
    RightParen,
}

fn expression_tokens(expression: &str) -> Result<Vec<Token>, String> {
    let mut value = expression.to_lowercase().replace(',', ".");
    for (phrase, replacement) in [
        ("разделить на", " / "),
        ("деленное на", " / "),
        ("делённое на", " / "),
        ("умножить на", " * "),
        ("в степени", " ^ "),
        ("плюс", " + "),
        ("минус", " - "),
        ("умножить", " * "),
        ("помножить", " * "),
        ("разделить", " / "),
    ] {
        value = value.replace(phrase, replacement);
    }
    let regex = Regex::new(r"\d+(?:\.\d+)?|[()+\-*/^]|[a-zа-яё]+").expect("valid tokenizer");
    let raw: Vec<&str> = regex.find_iter(&value).map(|item| item.as_str()).collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < raw.len() {
        let token = raw[index];
        if let Ok(number) = token.parse::<f64>() {
            tokens.push(Token::Number(number));
            index += 1;
            continue;
        }
        match token {
            "+" | "-" | "*" | "/" | "^" => {
                tokens.push(Token::Operator(token.chars().next().unwrap()));
                index += 1;
            }
            "(" => {
                tokens.push(Token::LeftParen);
                index += 1;
            }
            ")" => {
                tokens.push(Token::RightParen);
                index += 1;
            }
            _ => {
                let start = index;
                while index < raw.len()
                    && !matches!(raw[index], "+" | "-" | "*" | "/" | "^" | "(" | ")")
                    && raw[index].parse::<f64>().is_err()
                {
                    index += 1;
                }
                let number = parse_number_words(&raw[start..index]).ok_or_else(|| {
                    format!("неизвестное число `{}`", raw[start..index].join(" "))
                })?;
                tokens.push(Token::Number(number));
            }
        }
    }
    if tokens.is_empty() {
        return Err("пустое выражение".into());
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

impl Parser {
    fn expression(&mut self) -> Result<f64, String> {
        let mut value = self.term()?;
        while let Some(Token::Operator(operator @ ('+' | '-'))) = self.peek() {
            self.position += 1;
            let right = self.term()?;
            value = if operator == '+' {
                value + right
            } else {
                value - right
            };
        }
        Ok(value)
    }

    fn term(&mut self) -> Result<f64, String> {
        let mut value = self.power()?;
        while let Some(Token::Operator(operator @ ('*' | '/'))) = self.peek() {
            self.position += 1;
            let right = self.power()?;
            if operator == '/' && right == 0.0 {
                return Err("делить на ноль нельзя".into());
            }
            value = if operator == '*' {
                value * right
            } else {
                value / right
            };
        }
        Ok(value)
    }

    fn power(&mut self) -> Result<f64, String> {
        let left = self.unary()?;
        if matches!(self.peek(), Some(Token::Operator('^'))) {
            self.position += 1;
            Ok(left.powf(self.power()?))
        } else {
            Ok(left)
        }
    }

    fn unary(&mut self) -> Result<f64, String> {
        if let Some(Token::Operator(operator @ ('+' | '-'))) = self.peek() {
            self.position += 1;
            let value = self.unary()?;
            return Ok(if operator == '-' { -value } else { value });
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<f64, String> {
        match self.peek() {
            Some(Token::Number(value)) => {
                self.position += 1;
                Ok(value)
            }
            Some(Token::LeftParen) => {
                self.position += 1;
                let value = self.expression()?;
                if !matches!(self.peek(), Some(Token::RightParen)) {
                    return Err("не хватает закрывающей скобки".into());
                }
                self.position += 1;
                Ok(value)
            }
            _ => Err("ожидалось число".into()),
        }
    }

    fn peek(&self) -> Option<Token> {
        self.tokens.get(self.position).copied()
    }
}

fn format_number(value: f64) -> String {
    if (value.round() - value).abs() < 0.000_001 {
        return format!("{:.0}", value);
    }
    let formatted = format!("{value:.2}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_russian_spoken_numbers() {
        assert_eq!(parse_spoken_number("восемьдесят пять"), Some(85.0));
        assert_eq!(parse_spoken_number("две тысячи сто"), Some(2100.0));
    }

    #[test]
    fn calculates_spoken_expression_with_precedence() {
        let slots = calculate("два плюс три умножить на четыре", "answer").unwrap();
        assert_eq!(slots.get("answer").map(String::as_str), Some("14"));
        assert_eq!(
            slots.get("answer_phrase").map(String::as_str),
            Some("четырнадцать")
        );
    }

    #[test]
    fn rejects_division_by_zero() {
        assert!(calculate("10 разделить на ноль", "answer").is_err());
    }

    #[test]
    fn recognizes_common_currency_forms() {
        assert_eq!(currency("в рублях").map(|value| value.0), Some("RUB"));
        assert_eq!(currency("гривны").map(|value| value.0), Some("UAH"));
    }

    #[test]
    fn spells_numbers_in_russian_for_tts() {
        assert_eq!(number_to_russian_words(4.0), "четыре");
        assert_eq!(
            number_to_russian_words(1_242.0),
            "одна тысяча двести сорок два"
        );
        assert_eq!(
            number_to_russian_words(-12.5),
            "минус двенадцать целых пять десятых"
        );
    }

    #[test]
    fn weather_uses_explicit_location_then_fallback() {
        assert_eq!(
            resolved_location("Хамовники", "Москва").as_deref(),
            Some("Хамовники")
        );
        assert_eq!(
            resolved_location("{{location}}", "Москва").as_deref(),
            Some("Москва")
        );
    }

    #[test]
    fn maps_common_weather_codes() {
        assert_eq!(weather_code_description(0), "ясно");
        assert_eq!(weather_code_description(63), "дождь");
        assert_eq!(weather_code_description(95), "гроза");
    }
}
