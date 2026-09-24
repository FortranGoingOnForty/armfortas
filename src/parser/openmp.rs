//! OpenMP directive parser and construct association.

use std::collections::HashSet;

use super::{ParseError, Parser};
use crate::ast::openmp::{
    OpenMpClause, OpenMpConstruct, OpenMpDefault, OpenMpReductionOperator, OpenMpScheduleKind,
};
use crate::ast::stmt::{SpannedStmt, Stmt};
use crate::ast::Spanned;
use crate::lexer::{Lexer, Span, TokenKind};

#[derive(Debug)]
enum OpenMpHeader {
    Parallel(Vec<OpenMpClause>),
    Do(Vec<OpenMpClause>),
    ParallelDo(Vec<OpenMpClause>),
    Critical(Option<String>),
    EndParallel,
    EndDo { nowait: bool },
    EndParallelDo { nowait: bool },
    EndCritical(Option<String>),
}

#[derive(Debug, Clone, Copy)]
enum BeginKind {
    Parallel,
    Do,
    ParallelDo,
}

impl<'a> Parser<'a> {
    pub(crate) fn parse_openmp_construct(&mut self) -> Result<SpannedStmt, ParseError> {
        let directive = self.advance().clone();
        let header = parse_openmp_header(&directive.text, directive.span)?;

        match header {
            OpenMpHeader::Parallel(clauses) => {
                let (body, end_span) = self.parse_openmp_region_end(OpenMpEnd::Parallel, None)?;
                Ok(Spanned::new(
                    Stmt::OpenMp(OpenMpConstruct::Parallel { clauses, body }),
                    span_through(directive.span, end_span),
                ))
            }
            OpenMpHeader::Do(mut clauses) => {
                self.skip_newlines();
                let loop_stmt = self.parse_openmp_controlled_loop("do")?;
                let mut end_span = loop_stmt.span;
                self.skip_newlines();
                if let Some((span, nowait)) = self.consume_openmp_loop_end(false)? {
                    end_span = span;
                    if nowait {
                        clauses.push(OpenMpClause::Nowait);
                    }
                }
                Ok(Spanned::new(
                    Stmt::OpenMp(OpenMpConstruct::Do {
                        clauses,
                        loop_stmt: Box::new(loop_stmt),
                    }),
                    span_through(directive.span, end_span),
                ))
            }
            OpenMpHeader::ParallelDo(mut clauses) => {
                self.skip_newlines();
                let loop_stmt = self.parse_openmp_controlled_loop("parallel do")?;
                let mut end_span = loop_stmt.span;
                self.skip_newlines();
                if let Some((span, nowait)) = self.consume_openmp_loop_end(true)? {
                    end_span = span;
                    if nowait {
                        clauses.push(OpenMpClause::Nowait);
                    }
                }
                Ok(Spanned::new(
                    Stmt::OpenMp(OpenMpConstruct::ParallelDo {
                        clauses,
                        loop_stmt: Box::new(loop_stmt),
                    }),
                    span_through(directive.span, end_span),
                ))
            }
            OpenMpHeader::Critical(name) => {
                let (body, end_span) =
                    self.parse_openmp_region_end(OpenMpEnd::Critical, name.as_deref())?;
                Ok(Spanned::new(
                    Stmt::OpenMp(OpenMpConstruct::Critical { name, body }),
                    span_through(directive.span, end_span),
                ))
            }
            OpenMpHeader::EndParallel
            | OpenMpHeader::EndDo { .. }
            | OpenMpHeader::EndParallelDo { .. }
            | OpenMpHeader::EndCritical(_) => Err(ParseError {
                span: directive.span,
                msg: format!("unexpected OpenMP end directive '!$omp {}'", directive.text),
            }),
        }
    }

    fn parse_openmp_controlled_loop(
        &mut self,
        directive_name: &str,
    ) -> Result<SpannedStmt, ParseError> {
        let loop_stmt = self.parse_stmt()?;
        if !matches!(&loop_stmt.node, Stmt::DoLoop { .. }) {
            return Err(ParseError {
                span: loop_stmt.span,
                msg: format!("!$omp {directive_name} must be followed by a counted DO loop"),
            });
        }
        Ok(loop_stmt)
    }

    fn consume_openmp_loop_end(
        &mut self,
        combined_parallel: bool,
    ) -> Result<Option<(Span, bool)>, ParseError> {
        if self.peek() != &TokenKind::OmpDirective {
            return Ok(None);
        }
        let directive = self.current().clone();
        let header = parse_openmp_header(&directive.text, directive.span)?;
        let result = match (combined_parallel, header) {
            (false, OpenMpHeader::EndDo { nowait })
            | (true, OpenMpHeader::EndParallelDo { nowait }) => Some((directive.span, nowait)),
            _ => return Ok(None),
        };
        self.advance();
        Ok(result)
    }

    fn parse_openmp_region_end(
        &mut self,
        expected: OpenMpEnd,
        critical_name: Option<&str>,
    ) -> Result<(Vec<SpannedStmt>, Span), ParseError> {
        let mut body = Vec::new();
        loop {
            self.skip_newlines();
            if self.peek() == &TokenKind::Eof {
                return Err(self.error(format!(
                    "unterminated OpenMP {} construct; expected '!$omp end {}'",
                    expected.name(),
                    expected.name()
                )));
            }
            if self.peek() == &TokenKind::OmpDirective {
                let directive = self.current().clone();
                let header = parse_openmp_header(&directive.text, directive.span)?;
                let matches = match (&expected, &header) {
                    (OpenMpEnd::Parallel, OpenMpHeader::EndParallel) => true,
                    (OpenMpEnd::Critical, OpenMpHeader::EndCritical(end_name)) => {
                        let names_match = match (critical_name, end_name.as_deref()) {
                            (None, None) => true,
                            (Some(begin), Some(end)) => begin.eq_ignore_ascii_case(end),
                            _ => false,
                        };
                        if !names_match {
                            return Err(ParseError {
                                span: directive.span,
                                msg: format!(
                                    "OpenMP END CRITICAL name '{}' does not match '{}'",
                                    end_name.as_deref().unwrap_or("unnamed critical"),
                                    critical_name.unwrap_or("unnamed critical")
                                ),
                            });
                        }
                        true
                    }
                    _ => false,
                };
                if matches {
                    self.advance();
                    return Ok((body, directive.span));
                }
                if matches!(
                    header,
                    OpenMpHeader::EndParallel
                        | OpenMpHeader::EndDo { .. }
                        | OpenMpHeader::EndParallelDo { .. }
                        | OpenMpHeader::EndCritical(_)
                ) {
                    return Err(ParseError {
                        span: directive.span,
                        msg: format!(
                            "mismatched OpenMP end directive '!$omp {}'; expected END {}",
                            directive.text,
                            expected.name().to_ascii_uppercase()
                        ),
                    });
                }
            }
            body.push(self.parse_stmt()?);
        }
    }
}

#[derive(Debug)]
enum OpenMpEnd {
    Parallel,
    Critical,
}

impl OpenMpEnd {
    fn name(&self) -> &'static str {
        match self {
            Self::Parallel => "parallel",
            Self::Critical => "critical",
        }
    }
}

fn span_through(start: Span, end: Span) -> Span {
    Span {
        file_id: start.file_id,
        start: start.start,
        end: end.end,
    }
}

fn parse_openmp_header(source: &str, span: Span) -> Result<OpenMpHeader, ParseError> {
    let mut cursor = DirectiveCursor::new(source, span);
    let first = cursor
        .identifier("OpenMP directive name")?
        .to_ascii_lowercase();
    let header = match first.as_str() {
        "parallel" => {
            if cursor.peek_identifier_is("do") {
                cursor.identifier("DO")?;
                OpenMpHeader::ParallelDo(parse_clauses(&mut cursor, span, BeginKind::ParallelDo)?)
            } else {
                OpenMpHeader::Parallel(parse_clauses(&mut cursor, span, BeginKind::Parallel)?)
            }
        }
        "do" => OpenMpHeader::Do(parse_clauses(&mut cursor, span, BeginKind::Do)?),
        "critical" => {
            let name = if cursor.peek_nonblank() == Some(b'(') {
                Some(parse_single_name(
                    cursor.parenthesized("CRITICAL name")?,
                    span,
                    "CRITICAL name",
                )?)
            } else {
                None
            };
            cursor.finish()?;
            OpenMpHeader::Critical(name)
        }
        "end" => parse_openmp_end_header(&mut cursor)?,
        other => {
            return Err(ParseError {
                span,
                msg: format!("unsupported OpenMP directive '{other}'"),
            })
        }
    };
    Ok(header)
}

fn parse_openmp_end_header(cursor: &mut DirectiveCursor<'_>) -> Result<OpenMpHeader, ParseError> {
    let construct = cursor
        .identifier("construct name after OpenMP END")?
        .to_ascii_lowercase();
    match construct.as_str() {
        "parallel" if cursor.peek_identifier_is("do") => {
            cursor.identifier("DO")?;
            let nowait = parse_end_loop_clause(cursor)?;
            Ok(OpenMpHeader::EndParallelDo { nowait })
        }
        "parallel" => {
            cursor.finish()?;
            Ok(OpenMpHeader::EndParallel)
        }
        "do" => {
            let nowait = parse_end_loop_clause(cursor)?;
            Ok(OpenMpHeader::EndDo { nowait })
        }
        "critical" => {
            let name = if cursor.peek_nonblank() == Some(b'(') {
                Some(parse_single_name(
                    cursor.parenthesized("END CRITICAL name")?,
                    cursor.span,
                    "END CRITICAL name",
                )?)
            } else {
                None
            };
            cursor.finish()?;
            Ok(OpenMpHeader::EndCritical(name))
        }
        other => Err(cursor.error(format!("unsupported OpenMP END directive 'end {other}'"))),
    }
}

fn parse_end_loop_clause(cursor: &mut DirectiveCursor<'_>) -> Result<bool, ParseError> {
    if cursor.finished() {
        return Ok(false);
    }
    let clause = cursor.identifier("clause after OpenMP loop end")?;
    if !clause.eq_ignore_ascii_case("nowait") {
        return Err(cursor.error(format!(
            "unsupported clause '{clause}' on OpenMP loop end directive; expected NOWAIT"
        )));
    }
    cursor.finish()?;
    Ok(true)
}

fn parse_clauses(
    cursor: &mut DirectiveCursor<'_>,
    span: Span,
    construct: BeginKind,
) -> Result<Vec<OpenMpClause>, ParseError> {
    let mut clauses = Vec::new();
    let mut seen = HashSet::new();
    while !cursor.finished() {
        if cursor.peek_nonblank() == Some(b',') {
            cursor.pos += 1;
            if cursor.finished() {
                return Err(cursor.error("trailing comma in OpenMP clause list".into()));
            }
        }
        let name = cursor.identifier("OpenMP clause name")?;
        let lower = name.to_ascii_lowercase();
        // REDUCTION is repeatable; semantic validation diagnoses a list item
        // that appears in more than one data-sharing clause. Other clauses in
        // the currently modeled subset are unique.
        if lower != "reduction" && !seen.insert(lower.clone()) {
            return Err(cursor.error(format!("duplicate OpenMP {lower} clause")));
        }
        let clause = match lower.as_str() {
            "private" => OpenMpClause::Private(parse_name_list(
                cursor.parenthesized("PRIVATE list")?,
                span,
                "PRIVATE list",
            )?),
            "firstprivate" => OpenMpClause::FirstPrivate(parse_name_list(
                cursor.parenthesized("FIRSTPRIVATE list")?,
                span,
                "FIRSTPRIVATE list",
            )?),
            "shared" => OpenMpClause::Shared(parse_name_list(
                cursor.parenthesized("SHARED list")?,
                span,
                "SHARED list",
            )?),
            "default" => {
                let value = cursor.parenthesized("DEFAULT clause")?.trim();
                let value = match value.to_ascii_lowercase().as_str() {
                    "shared" => OpenMpDefault::Shared,
                    "private" => OpenMpDefault::Private,
                    "firstprivate" => OpenMpDefault::FirstPrivate,
                    "none" => OpenMpDefault::None,
                    _ => {
                        return Err(cursor.error(format!("invalid OpenMP DEFAULT value '{value}'")))
                    }
                };
                OpenMpClause::Default(value)
            }
            "if" => {
                let value = cursor.parenthesized("IF clause")?;
                let (modifier, expression) = split_if_modifier(value);
                OpenMpClause::If {
                    modifier,
                    condition: parse_clause_expr(expression, span, "IF clause")?,
                }
            }
            "num_threads" => OpenMpClause::NumThreads(parse_clause_expr(
                cursor.parenthesized("NUM_THREADS clause")?,
                span,
                "NUM_THREADS clause",
            )?),
            "schedule" => {
                let value = cursor.parenthesized("SCHEDULE clause")?;
                let parts = split_top_level(value, ',');
                if parts.is_empty() || parts.len() > 2 {
                    return Err(
                        cursor.error("SCHEDULE requires a kind and at most one chunk size".into())
                    );
                }
                let kind_text = parts[0].trim();
                let kind = match kind_text.to_ascii_lowercase().as_str() {
                    "static" => OpenMpScheduleKind::Static,
                    "dynamic" => OpenMpScheduleKind::Dynamic,
                    "guided" => OpenMpScheduleKind::Guided,
                    "runtime" => OpenMpScheduleKind::Runtime,
                    "auto" => OpenMpScheduleKind::Auto,
                    _ => {
                        return Err(
                            cursor.error(format!("unsupported OpenMP schedule kind '{kind_text}'"))
                        )
                    }
                };
                let chunk_size = parts
                    .get(1)
                    .map(|value| parse_clause_expr(value, span, "SCHEDULE chunk size"))
                    .transpose()?;
                OpenMpClause::Schedule { kind, chunk_size }
            }
            "collapse" => OpenMpClause::Collapse(parse_clause_expr(
                cursor.parenthesized("COLLAPSE clause")?,
                span,
                "COLLAPSE clause",
            )?),
            "reduction" => {
                let value = cursor.parenthesized("REDUCTION clause")?;
                let Some((operator, variables)) = split_top_level_once(value, ':') else {
                    return Err(
                        cursor.error("OpenMP REDUCTION requires 'operator: variable-list'".into())
                    );
                };
                OpenMpClause::Reduction {
                    operator: parse_reduction_operator(operator, cursor)?,
                    variables: parse_name_list(variables, span, "REDUCTION list")?,
                }
            }
            "nowait" => {
                return Err(cursor.error(
                    "OpenMP NOWAIT belongs on the END DO or END PARALLEL DO directive".into(),
                ))
            }
            _ => return Err(cursor.error(format!("unsupported OpenMP clause '{name}'"))),
        };
        validate_clause_for_construct(&clause, construct, cursor)?;
        clauses.push(clause);
    }
    Ok(clauses)
}

fn validate_clause_for_construct(
    clause: &OpenMpClause,
    construct: BeginKind,
    cursor: &DirectiveCursor<'_>,
) -> Result<(), ParseError> {
    let allowed = match construct {
        BeginKind::Parallel => matches!(
            clause,
            OpenMpClause::Private(_)
                | OpenMpClause::FirstPrivate(_)
                | OpenMpClause::Shared(_)
                | OpenMpClause::Default(_)
                | OpenMpClause::If { .. }
                | OpenMpClause::NumThreads(_)
                | OpenMpClause::Reduction { .. }
        ),
        BeginKind::Do => matches!(
            clause,
            OpenMpClause::Private(_)
                | OpenMpClause::FirstPrivate(_)
                | OpenMpClause::Schedule { .. }
                | OpenMpClause::Collapse(_)
                | OpenMpClause::Reduction { .. }
        ),
        BeginKind::ParallelDo => !matches!(clause, OpenMpClause::Nowait),
    };
    if allowed {
        Ok(())
    } else {
        Err(cursor.error("OpenMP clause is not valid on this construct".into()))
    }
}

fn parse_reduction_operator(
    source: &str,
    cursor: &DirectiveCursor<'_>,
) -> Result<OpenMpReductionOperator, ParseError> {
    match source.trim().to_ascii_lowercase().as_str() {
        "+" => Ok(OpenMpReductionOperator::Add),
        "*" => Ok(OpenMpReductionOperator::Multiply),
        "max" => Ok(OpenMpReductionOperator::Max),
        "min" => Ok(OpenMpReductionOperator::Min),
        ".and." => Ok(OpenMpReductionOperator::And),
        ".or." => Ok(OpenMpReductionOperator::Or),
        ".eqv." => Ok(OpenMpReductionOperator::Eqv),
        ".neqv." => Ok(OpenMpReductionOperator::Neqv),
        other => Err(cursor.error(format!("unsupported OpenMP reduction operator '{other}'"))),
    }
}

fn parse_name_list(source: &str, span: Span, context: &str) -> Result<Vec<String>, ParseError> {
    let values = split_top_level(source, ',');
    if values.is_empty() || values.iter().any(|value| value.trim().is_empty()) {
        return Err(ParseError {
            span,
            msg: format!("OpenMP {context} must not be empty"),
        });
    }
    values
        .into_iter()
        .map(|value| parse_single_name(value, span, context))
        .collect()
}

fn parse_single_name(source: &str, span: Span, context: &str) -> Result<String, ParseError> {
    let name = source.trim();
    let valid = !name.is_empty()
        && name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if !valid {
        return Err(ParseError {
            span,
            msg: format!("unsupported OpenMP list item '{name}' in {context}"),
        });
    }
    Ok(name.to_string())
}

fn parse_clause_expr(
    source: &str,
    span: Span,
    context: &str,
) -> Result<crate::ast::expr::SpannedExpr, ParseError> {
    if source.trim().is_empty() {
        return Err(ParseError {
            span,
            msg: format!("OpenMP {context} requires an expression"),
        });
    }
    let mut tokens = Lexer::tokenize(source.trim(), span.file_id).map_err(|error| ParseError {
        span,
        msg: format!("invalid expression in OpenMP {context}: {}", error.msg),
    })?;
    for token in &mut tokens {
        token.span = span;
    }
    let mut parser = Parser::new(&tokens);
    let expression = parser.parse_expr().map_err(|error| ParseError {
        span,
        msg: format!("invalid expression in OpenMP {context}: {}", error.msg),
    })?;
    if parser.peek() != &TokenKind::Eof {
        return Err(ParseError {
            span,
            msg: format!("unexpected text in OpenMP {context}"),
        });
    }
    Ok(expression)
}

fn split_if_modifier(source: &str) -> (Option<String>, &str) {
    if let Some((prefix, expression)) = split_top_level_once(source, ':') {
        let prefix = prefix.trim();
        if prefix.eq_ignore_ascii_case("parallel") {
            return (Some(prefix.to_ascii_lowercase()), expression);
        }
    }
    (None, source)
}

fn split_top_level(source: &str, delimiter: char) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut start = 0;
    for offset in top_level_delimiter_offsets(source, delimiter) {
        pieces.push(&source[start..offset]);
        start = offset + delimiter.len_utf8();
    }
    pieces.push(&source[start..]);
    pieces
}

fn top_level_delimiter_offsets(source: &str, delimiter: char) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut depth = 0usize;
    let mut quote = None;
    let chars = source.char_indices().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        let (offset, ch) = chars[index];
        match quote {
            Some(active) if ch == active => {
                if chars
                    .get(index + 1)
                    .is_some_and(|(_, next)| *next == active)
                {
                    index += 1;
                } else {
                    quote = None;
                }
            }
            Some(_) => {}
            None if matches!(ch, '\'' | '"') => quote = Some(ch),
            None if ch == '(' => depth += 1,
            None if ch == ')' => depth = depth.saturating_sub(1),
            None if ch == delimiter && depth == 0 => {
                offsets.push(offset);
            }
            None => {}
        }
        index += 1;
    }
    offsets
}

fn split_top_level_once(source: &str, delimiter: char) -> Option<(&str, &str)> {
    let offsets = top_level_delimiter_offsets(source, delimiter);
    if offsets.len() != 1 {
        return None;
    }
    let offset = offsets[0];
    Some((&source[..offset], &source[offset + delimiter.len_utf8()..]))
}

struct DirectiveCursor<'a> {
    source: &'a str,
    pos: usize,
    span: Span,
}

impl<'a> DirectiveCursor<'a> {
    fn new(source: &'a str, span: Span) -> Self {
        Self {
            source,
            pos: 0,
            span,
        }
    }

    fn skip_blanks(&mut self) {
        while self
            .source
            .as_bytes()
            .get(self.pos)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.pos += 1;
        }
    }

    fn peek_nonblank(&mut self) -> Option<u8> {
        self.skip_blanks();
        self.source.as_bytes().get(self.pos).copied()
    }

    fn finished(&mut self) -> bool {
        self.skip_blanks();
        self.pos == self.source.len()
    }

    fn finish(&mut self) -> Result<(), ParseError> {
        if self.finished() {
            Ok(())
        } else {
            Err(self.error(format!(
                "unexpected text '{}' in OpenMP directive",
                &self.source[self.pos..]
            )))
        }
    }

    fn identifier(&mut self, expected: &str) -> Result<&'a str, ParseError> {
        self.skip_blanks();
        let start = self.pos;
        while self
            .source
            .as_bytes()
            .get(self.pos)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            self.pos += 1;
        }
        if self.pos == start {
            Err(self.error(format!("expected {expected} in OpenMP directive")))
        } else {
            Ok(&self.source[start..self.pos])
        }
    }

    fn peek_identifier_is(&mut self, expected: &str) -> bool {
        let saved = self.pos;
        let result = self
            .identifier("identifier")
            .is_ok_and(|name| name.eq_ignore_ascii_case(expected));
        self.pos = saved;
        result
    }

    fn parenthesized(&mut self, context: &str) -> Result<&'a str, ParseError> {
        self.skip_blanks();
        if self.source.as_bytes().get(self.pos) != Some(&b'(') {
            return Err(self.error(format!("OpenMP {context} requires parentheses")));
        }
        self.pos += 1;
        let start = self.pos;
        let mut depth = 1usize;
        let mut quote = None;
        while let Some(&byte) = self.source.as_bytes().get(self.pos) {
            match quote {
                Some(active) if byte == active => {
                    if self.source.as_bytes().get(self.pos + 1) == Some(&active) {
                        self.pos += 2;
                        continue;
                    }
                    quote = None;
                }
                Some(_) => {}
                None if matches!(byte, b'\'' | b'"') => quote = Some(byte),
                None if byte == b'(' => depth += 1,
                None if byte == b')' => {
                    depth -= 1;
                    if depth == 0 {
                        let end = self.pos;
                        self.pos += 1;
                        return Ok(&self.source[start..end]);
                    }
                }
                None => {}
            }
            self.pos += 1;
        }
        Err(self.error(format!("unterminated parentheses in OpenMP {context}")))
    }

    fn error(&self, msg: String) -> ParseError {
        ParseError {
            span: self.span,
            msg,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::{tokenize_with_options, LexerOptions, SourceForm};

    fn parse(source: &str, form: SourceForm) -> Result<SpannedStmt, ParseError> {
        let tokens = tokenize_with_options(source, 0, form, LexerOptions { openmp: true }).unwrap();
        Parser::new_for_form(&tokens, form).parse_stmt()
    }

    #[test]
    fn parses_parallel_region_with_typed_clauses() {
        let stmt = parse(
            "!$omp parallel default(shared) private(i) num_threads(n) if(n > 1)\n\
             i = 1\n\
             !$omp end parallel\n",
            SourceForm::FreeForm,
        )
        .unwrap();
        let Stmt::OpenMp(OpenMpConstruct::Parallel { clauses, body }) = stmt.node else {
            panic!("expected parallel construct");
        };
        assert_eq!(clauses.len(), 4);
        assert_eq!(body.len(), 1);
    }

    #[test]
    fn parses_ferp_parallel_do_clause_shape() {
        let stmt = parse(
            "!$omp parallel do default(shared) private(src,file_match) &\n\
             !$omp& reduction(.or.:any_match,has_error) schedule(dynamic)\n\
             do i = 1, n\n\
               any_match = .true.\n\
             end do\n\
             !$omp end parallel do nowait\n",
            SourceForm::FreeForm,
        )
        .unwrap();
        let Stmt::OpenMp(OpenMpConstruct::ParallelDo { clauses, loop_stmt }) = stmt.node else {
            panic!("expected parallel-do construct");
        };
        assert!(matches!(loop_stmt.node, Stmt::DoLoop { .. }));
        assert!(clauses.iter().any(|clause| matches!(
            clause,
            OpenMpClause::Reduction {
                operator: OpenMpReductionOperator::Or,
                variables,
            } if variables == &["any_match", "has_error"]
        )));
        assert!(clauses
            .iter()
            .any(|clause| matches!(clause, OpenMpClause::Nowait)));
    }

    #[test]
    fn parses_multiple_reduction_clauses() {
        let stmt = parse(
            "!$omp parallel do reduction(+:total) reduction(max:largest) &\n\
             !$omp& reduction(.or.:failed)\n\
             do i = 1, n\n\
               total = total + i\n\
             end do\n\
             !$omp end parallel do\n",
            SourceForm::FreeForm,
        )
        .unwrap();
        let Stmt::OpenMp(OpenMpConstruct::ParallelDo { clauses, .. }) = stmt.node else {
            panic!("expected parallel-do construct");
        };
        assert_eq!(
            clauses
                .iter()
                .filter(|clause| matches!(clause, OpenMpClause::Reduction { .. }))
                .count(),
            3
        );
    }

    #[test]
    fn parses_named_fixed_form_critical_region() {
        let stmt = parse(
            "C$OMP CRITICAL(ERROR_OUTPUT)\n      X = 1\nC$OMP END CRITICAL(ERROR_OUTPUT)\n",
            SourceForm::FixedForm,
        )
        .unwrap();
        assert!(matches!(
            stmt.node,
            Stmt::OpenMp(OpenMpConstruct::Critical {
                name: Some(ref name),
                ref body,
            }) if name.eq_ignore_ascii_case("error_output") && body.len() == 1
        ));
    }

    #[test]
    fn parses_worksharing_do_clauses_and_end_nowait() {
        let stmt = parse(
            "!$omp do private(i), firstprivate(chunk) schedule(static, chunk) collapse(2)\n\
             do i = 1, n\n\
               do j = 1, n\n\
               end do\n\
             end do\n\
             !$omp end do nowait\n",
            SourceForm::FreeForm,
        )
        .unwrap();
        let Stmt::OpenMp(OpenMpConstruct::Do { clauses, .. }) = stmt.node else {
            panic!("expected worksharing-do construct");
        };
        assert!(clauses.iter().any(
            |clause| matches!(clause, OpenMpClause::FirstPrivate(names) if names == &["chunk"])
        ));
        assert!(clauses.iter().any(|clause| matches!(
            clause,
            OpenMpClause::Schedule {
                kind: OpenMpScheduleKind::Static,
                chunk_size: Some(_),
            }
        )));
        assert!(clauses
            .iter()
            .any(|clause| matches!(clause, OpenMpClause::Collapse(_))));
        assert!(clauses
            .iter()
            .any(|clause| matches!(clause, OpenMpClause::Nowait)));
    }

    #[test]
    fn rejects_mismatched_critical_name() {
        let error = parse(
            "!$omp critical(output)\nx = 1\n!$omp end critical(error)\n",
            SourceForm::FreeForm,
        )
        .unwrap_err();
        assert!(error.msg.contains("does not match"), "{error}");
    }

    #[test]
    fn rejects_missing_end_critical_name() {
        let error = parse(
            "!$omp critical(output)\nx = 1\n!$omp end critical\n",
            SourceForm::FreeForm,
        )
        .unwrap_err();
        assert!(error.msg.contains("does not match"), "{error}");
    }

    #[test]
    fn rejects_non_loop_after_worksharing_do() {
        let error = parse("!$omp do\nx = 1\n", SourceForm::FreeForm).unwrap_err();
        assert!(error.msg.contains("counted DO loop"), "{error}");
    }

    #[test]
    fn rejects_unsupported_clause_without_erasing_it() {
        let error = parse(
            "!$omp parallel proc_bind(close)\n!$omp end parallel\n",
            SourceForm::FreeForm,
        )
        .unwrap_err();
        assert!(error.msg.contains("unsupported OpenMP clause 'proc_bind'"));
    }
}
