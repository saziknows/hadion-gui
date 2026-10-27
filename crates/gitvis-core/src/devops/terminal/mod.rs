//! Terminal sessions: a pseudo-terminal (`portable-pty`; ConPTY on Windows
//! asks for the cursor position with `ESC[6n` and waits for the answer
//! before it sends anything else) with a VT100 screen (`vt100`), shells
//! found on this machine, keyboard input encoding.
