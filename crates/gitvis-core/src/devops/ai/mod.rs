//! AI assist through the Claude API (`curl`, the key on standard input,
//! never on a command line): commit messages from the staged diff, pull
//! request descriptions, explaining a failing CI log, summarising a pull
//! request; request building, SSE stream parsing, usage and cost, and
//! redacting secrets from anything sent.
