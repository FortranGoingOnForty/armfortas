.global _main

.text
.p2align 2

_main:
    stp x29, x30, [sp, #-16]!
    mov x29, sp

    mov x0, #0
    bl _isatty
    cbnz x0, .Lhave_tty

    adrp x0, _not_tty_msg@PAGE
    add x0, x0, _not_tty_msg@PAGEOFF
    bl _puts
    mov x0, #1
    bl _exit

.Lhave_tty:
    bl setup_terminal
    cbz x0, .Lsetup_ok

    adrp x0, _setup_fail_msg@PAGE
    add x0, x0, _setup_fail_msg@PAGEOFF
    bl _puts
    mov x0, #1
    bl _exit

.Lsetup_ok:
    bl init_game
    bl draw_frame

.Lgame_loop:
    movz x0, #61, lsl #12
    add x0, x0, #144
    bl _usleep

    bl poll_input
    adrp x9, _game_state@PAGE
    add x9, x9, _game_state@PAGEOFF
    ldr x10, [x9]
    cbnz x10, .Lfinish

    bl advance_game
    bl draw_frame

    adrp x9, _game_state@PAGE
    add x9, x9, _game_state@PAGEOFF
    ldr x10, [x9]
    cbz x10, .Lgame_loop

.Lfinish:
    adrp x9, _game_state@PAGE
    add x9, x9, _game_state@PAGEOFF
    ldr x19, [x9]

    adrp x20, _score_value@PAGE
    add x20, x20, _score_value@PAGEOFF
    ldr x21, [x20]

    cmp x19, #2
    b.eq .Lprint_win
    cmp x19, #3
    b.eq .Lprint_quit

    adrp x0, _lose_fmt@PAGE
    add x0, x0, _lose_fmt@PAGEOFF
    mov x1, #24
    mov x2, x21
    bl write_message_with_score
    b .Lexit_clean

.Lprint_win:
    adrp x0, _win_fmt@PAGE
    add x0, x0, _win_fmt@PAGEOFF
    mov x1, #44
    mov x2, x21
    bl write_message_with_score
    b .Lexit_clean

.Lprint_quit:
    adrp x0, _quit_fmt@PAGE
    add x0, x0, _quit_fmt@PAGEOFF
    mov x1, #19
    mov x2, x21
    bl write_message_with_score

.Lexit_clean:
    mov x0, #0
    bl _fflush
    mov x0, #0
    bl _exit

setup_terminal:
    stp x29, x30, [sp, #-32]!
    stp x19, x20, [sp, #16]
    mov x29, sp

    mov x0, #0
    mov x1, #3
    mov x2, #0
    bl _fcntl
    cmp x0, #0
    b.lt .Lsetup_fail

    adrp x19, _saved_flags@PAGE
    add x19, x19, _saved_flags@PAGEOFF
    str x0, [x19]

    adrp x19, _flags_valid@PAGE
    add x19, x19, _flags_valid@PAGEOFF
    mov x0, #1
    str x0, [x19]

    adrp x19, _original_termios@PAGE
    add x19, x19, _original_termios@PAGEOFF
    mov x0, #0
    mov x1, x19
    bl _tcgetattr
    cbnz x0, .Lsetup_fail

    adrp x20, _raw_termios@PAGE
    add x20, x20, _raw_termios@PAGEOFF

    ldr x0, [x19, #0]
    str x0, [x20, #0]
    ldr x0, [x19, #8]
    str x0, [x20, #8]
    ldr x0, [x19, #16]
    str x0, [x20, #16]
    ldr x0, [x19, #24]
    str x0, [x20, #24]
    ldr x0, [x19, #32]
    str x0, [x20, #32]
    ldr x0, [x19, #40]
    str x0, [x20, #40]
    ldr x0, [x19, #48]
    str x0, [x20, #48]
    ldr x0, [x19, #56]
    str x0, [x20, #56]
    ldr x0, [x19, #64]
    str x0, [x20, #64]

    mov x0, x20
    bl _cfmakeraw

    ldr x0, [x20, #48]
    movn x1, #0xffff
    and x0, x0, x1
    str x0, [x20, #48]

    mov x0, #0
    mov x1, #0
    mov x2, x20
    bl _tcsetattr
    cbnz x0, .Lsetup_fail

    adrp x19, _terminal_active@PAGE
    add x19, x19, _terminal_active@PAGEOFF
    mov x0, #1
    str x0, [x19]

    adrp x0, restore_terminal@PAGE
    add x0, x0, restore_terminal@PAGEOFF
    bl _atexit

    adrp x19, _saved_flags@PAGE
    add x19, x19, _saved_flags@PAGEOFF
    ldr x2, [x19]
    mov x3, #4
    orr x2, x2, x3
    mov x0, #0
    mov x1, #4
    bl _fcntl
    cmp x0, #0
    b.lt .Lsetup_fail

    adrp x0, _clear_hide_seq@PAGE
    add x0, x0, _clear_hide_seq@PAGEOFF
    mov x1, #13
    bl write_stdout

    mov x0, #0
    b .Lsetup_done

.Lsetup_fail:
    mov x0, #1

.Lsetup_done:
    ldp x19, x20, [sp, #16]
    ldp x29, x30, [sp], #32
    ret

restore_terminal:
    stp x29, x30, [sp, #-32]!
    stp x19, x20, [sp, #16]
    mov x29, sp

    adrp x19, _terminal_active@PAGE
    add x19, x19, _terminal_active@PAGEOFF
    ldr x0, [x19]
    cbz x0, .Lskip_term_restore

    adrp x20, _original_termios@PAGE
    add x20, x20, _original_termios@PAGEOFF
    mov x0, #0
    mov x1, #0
    mov x2, x20
    bl _tcsetattr

    mov x0, #0
    str x0, [x19]

.Lskip_term_restore:
    adrp x19, _flags_valid@PAGE
    add x19, x19, _flags_valid@PAGEOFF
    ldr x0, [x19]
    cbz x0, .Lskip_flag_restore

    adrp x20, _saved_flags@PAGE
    add x20, x20, _saved_flags@PAGEOFF
    ldr x2, [x20]
    mov x0, #0
    mov x1, #4
    bl _fcntl

    mov x0, #0
    str x0, [x19]

.Lskip_flag_restore:
    adrp x0, _restore_seq@PAGE
    add x0, x0, _restore_seq@PAGEOFF
    mov x1, #11
    bl write_stdout

    ldp x19, x20, [sp, #16]
    ldp x29, x30, [sp], #32
    ret

write_stdout:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    mov x2, x1
    mov x1, x0
    mov x0, #1
    bl _write
    ldp x29, x30, [sp], #16
    ret

write_char_stdout:
    stp x29, x30, [sp, #-32]!
    mov x29, sp

    str x0, [sp, #16]
    add x0, sp, #16
    mov x1, #1
    bl write_stdout

    ldp x29, x30, [sp], #32
    ret

write_u64_stdout:
    stp x29, x30, [sp, #-208]!
    stp x19, x20, [sp, #16]
    stp x21, x22, [sp, #32]
    mov x29, sp

    mov x19, x0
    cbnz x19, .Lwrite_u64_loop_setup

    mov x0, #48
    bl write_char_stdout
    b .Lwrite_u64_done

.Lwrite_u64_loop_setup:
    add x20, sp, #48
    mov x21, #0

.Lwrite_u64_div_loop:
    mov x22, #10
    udiv x9, x19, x22
    mul x10, x9, x22
    sub x10, x19, x10
    add x10, x10, #48
    str x10, [x20, x21, lsl #3]
    mov x19, x9
    add x21, x21, #1
    cbnz x19, .Lwrite_u64_div_loop

.Lwrite_u64_emit_loop:
    sub x21, x21, #1
    ldr x0, [x20, x21, lsl #3]
    bl write_char_stdout
    cbnz x21, .Lwrite_u64_emit_loop

.Lwrite_u64_done:
    ldp x21, x22, [sp, #32]
    ldp x19, x20, [sp, #16]
    ldp x29, x30, [sp], #208
    ret

write_message_with_score:
    stp x29, x30, [sp, #-48]!
    stp x19, x20, [sp, #16]
    stp x21, x22, [sp, #32]
    mov x29, sp

    mov x19, x0
    mov x20, x1
    mov x21, x2

    adrp x0, _crlf@PAGE
    add x0, x0, _crlf@PAGEOFF
    mov x1, #2
    bl write_stdout

    mov x0, x19
    mov x1, x20
    bl write_stdout

    mov x0, x21
    bl write_u64_stdout

    adrp x0, _crlf@PAGE
    add x0, x0, _crlf@PAGEOFF
    mov x1, #2
    bl write_stdout

    ldp x21, x22, [sp, #32]
    ldp x19, x20, [sp, #16]
    ldp x29, x30, [sp], #48
    ret

init_game:
    stp x29, x30, [sp, #-16]!
    mov x29, sp

    adrp x9, _snake_length@PAGE
    add x9, x9, _snake_length@PAGEOFF
    mov x10, #4
    str x10, [x9]

    adrp x9, _snake_dir_x@PAGE
    add x9, x9, _snake_dir_x@PAGEOFF
    mov x10, #1
    str x10, [x9]

    adrp x9, _snake_dir_y@PAGE
    add x9, x9, _snake_dir_y@PAGEOFF
    mov x10, #0
    str x10, [x9]

    adrp x9, _score_value@PAGE
    add x9, x9, _score_value@PAGEOFF
    mov x10, #0
    str x10, [x9]

    adrp x9, _game_state@PAGE
    add x9, x9, _game_state@PAGEOFF
    mov x10, #0
    str x10, [x9]

    adrp x9, _food_index@PAGE
    add x9, x9, _food_index@PAGEOFF
    mov x10, #0
    str x10, [x9]

    adrp x11, _snake_x@PAGE
    add x11, x11, _snake_x@PAGEOFF
    mov x10, #20
    str x10, [x11, #0]
    mov x10, #19
    str x10, [x11, #8]
    mov x10, #18
    str x10, [x11, #16]
    mov x10, #17
    str x10, [x11, #24]

    adrp x12, _snake_y@PAGE
    add x12, x12, _snake_y@PAGEOFF
    mov x10, #8
    str x10, [x12, #0]
    str x10, [x12, #8]
    str x10, [x12, #16]
    str x10, [x12, #24]

    bl spawn_food

    ldp x29, x30, [sp], #16
    ret

spawn_food:
    stp x29, x30, [sp, #-16]!
    mov x29, sp

    adrp x9, _food_index@PAGE
    add x9, x9, _food_index@PAGEOFF
    ldr x10, [x9]
    lsl x11, x10, #4

    adrp x12, _food_table@PAGE
    add x12, x12, _food_table@PAGEOFF

    ldr x13, [x12, x11]
    add x14, x11, #8
    ldr x15, [x12, x14]

    adrp x9, _food_x@PAGE
    add x9, x9, _food_x@PAGEOFF
    str x13, [x9]

    adrp x9, _food_y@PAGE
    add x9, x9, _food_y@PAGEOFF
    str x15, [x9]

    ldp x29, x30, [sp], #16
    ret

poll_input:
    stp x29, x30, [sp, #-32]!
    stp x19, x20, [sp, #16]
    mov x29, sp

    adrp x19, _input_byte@PAGE
    add x19, x19, _input_byte@PAGEOFF

.Lpoll_loop:
    mov x0, #0
    mov x1, x19
    mov x2, #1
    bl _read
    cmp x0, #1
    b.ne .Lpoll_done

    ldrb w20, [x19]

    cmp x20, #113
    b.eq .Lset_quit
    cmp x20, #81
    b.eq .Lset_quit

    cmp x20, #119
    b.eq .Lgo_up
    cmp x20, #87
    b.eq .Lgo_up

    cmp x20, #115
    b.eq .Lgo_down
    cmp x20, #83
    b.eq .Lgo_down

    cmp x20, #97
    b.eq .Lgo_left
    cmp x20, #65
    b.eq .Lgo_left

    cmp x20, #100
    b.eq .Lgo_right
    cmp x20, #68
    b.eq .Lgo_right

    b .Lpoll_done

.Lset_quit:
    adrp x9, _game_state@PAGE
    add x9, x9, _game_state@PAGEOFF
    mov x10, #3
    str x10, [x9]
    b .Lpoll_done

.Lgo_up:
    adrp x9, _snake_dir_y@PAGE
    add x9, x9, _snake_dir_y@PAGEOFF
    ldr x10, [x9]
    cmp x10, #1
    b.eq .Lpoll_done

    adrp x9, _snake_dir_x@PAGE
    add x9, x9, _snake_dir_x@PAGEOFF
    mov x10, #0
    str x10, [x9]

    adrp x9, _snake_dir_y@PAGE
    add x9, x9, _snake_dir_y@PAGEOFF
    mov x10, #-1
    str x10, [x9]
    b .Lpoll_done

.Lgo_down:
    adrp x9, _snake_dir_y@PAGE
    add x9, x9, _snake_dir_y@PAGEOFF
    ldr x10, [x9]
    mov x11, #-1
    cmp x10, x11
    b.eq .Lpoll_done

    adrp x9, _snake_dir_x@PAGE
    add x9, x9, _snake_dir_x@PAGEOFF
    mov x10, #0
    str x10, [x9]

    adrp x9, _snake_dir_y@PAGE
    add x9, x9, _snake_dir_y@PAGEOFF
    mov x10, #1
    str x10, [x9]
    b .Lpoll_done

.Lgo_left:
    adrp x9, _snake_dir_x@PAGE
    add x9, x9, _snake_dir_x@PAGEOFF
    ldr x10, [x9]
    cmp x10, #1
    b.eq .Lpoll_done

    adrp x9, _snake_dir_x@PAGE
    add x9, x9, _snake_dir_x@PAGEOFF
    mov x10, #-1
    str x10, [x9]

    adrp x9, _snake_dir_y@PAGE
    add x9, x9, _snake_dir_y@PAGEOFF
    mov x10, #0
    str x10, [x9]
    b .Lpoll_done

.Lgo_right:
    adrp x9, _snake_dir_x@PAGE
    add x9, x9, _snake_dir_x@PAGEOFF
    ldr x10, [x9]
    mov x11, #-1
    cmp x10, x11
    b.eq .Lpoll_done

    adrp x9, _snake_dir_x@PAGE
    add x9, x9, _snake_dir_x@PAGEOFF
    mov x10, #1
    str x10, [x9]

    adrp x9, _snake_dir_y@PAGE
    add x9, x9, _snake_dir_y@PAGEOFF
    mov x10, #0
    str x10, [x9]
    b .Lpoll_done

.Lpoll_done:
    ldp x19, x20, [sp, #16]
    ldp x29, x30, [sp], #32
    ret

advance_game:
    stp x29, x30, [sp, #-96]!
    stp x19, x20, [sp, #16]
    stp x21, x22, [sp, #32]
    stp x23, x24, [sp, #48]
    stp x25, x26, [sp, #64]
    stp x27, x28, [sp, #80]
    mov x29, sp

    adrp x19, _snake_x@PAGE
    add x19, x19, _snake_x@PAGEOFF
    adrp x20, _snake_y@PAGE
    add x20, x20, _snake_y@PAGEOFF

    ldr x21, [x19, #0]
    ldr x22, [x20, #0]

    adrp x9, _snake_dir_x@PAGE
    add x9, x9, _snake_dir_x@PAGEOFF
    ldr x23, [x9]

    adrp x9, _snake_dir_y@PAGE
    add x9, x9, _snake_dir_y@PAGEOFF
    ldr x24, [x9]

    orr x9, x23, x24
    cbz x9, .Ladvance_done

    add x25, x21, x23
    add x26, x22, x24

    cmp x25, #0
    b.lt .Llose_now
    cmp x25, #40
    b.ge .Llose_now
    cmp x26, #0
    b.lt .Llose_now
    cmp x26, #16
    b.ge .Llose_now

    mov x27, #0

    adrp x9, _food_x@PAGE
    add x9, x9, _food_x@PAGEOFF
    ldr x10, [x9]
    cmp x25, x10
    b.ne .Lgrowth_checked

    adrp x9, _food_y@PAGE
    add x9, x9, _food_y@PAGEOFF
    ldr x10, [x9]
    cmp x26, x10
    b.ne .Lgrowth_checked

    mov x27, #1

.Lgrowth_checked:
    adrp x9, _snake_length@PAGE
    add x9, x9, _snake_length@PAGEOFF
    ldr x21, [x9]

    cbz x27, .Lnormal_shift_setup
    add x22, x21, #1
    mov x23, x21
    b .Lshift_loop_start

.Lnormal_shift_setup:
    mov x22, x21
    sub x23, x21, #1

.Lshift_loop_start:
    cmp x23, #1
    b.lt .Lshift_done

.Lshift_loop:
    sub x24, x23, #1
    ldr x10, [x19, x24, lsl #3]
    str x10, [x19, x23, lsl #3]
    ldr x10, [x20, x24, lsl #3]
    str x10, [x20, x23, lsl #3]
    sub x23, x23, #1
    cmp x23, #1
    b.ge .Lshift_loop

.Lshift_done:
    str x25, [x19, #0]
    str x26, [x20, #0]

    adrp x9, _snake_length@PAGE
    add x9, x9, _snake_length@PAGEOFF
    str x22, [x9]

    mov x23, #1

.Lself_check:
    cmp x23, x22
    b.ge .Lafter_self_check
    ldr x10, [x19, x23, lsl #3]
    cmp x10, x25
    b.ne .Lnext_segment
    ldr x10, [x20, x23, lsl #3]
    cmp x10, x26
    b.eq .Llose_now

.Lnext_segment:
    add x23, x23, #1
    b .Lself_check

.Lafter_self_check:
    cbz x27, .Ladvance_done

    adrp x9, _score_value@PAGE
    add x9, x9, _score_value@PAGEOFF
    ldr x10, [x9]
    add x10, x10, #1
    str x10, [x9]

    adrp x9, _food_index@PAGE
    add x9, x9, _food_index@PAGEOFF
    ldr x10, [x9]
    add x10, x10, #1
    str x10, [x9]
    cmp x10, #8
    b.ge .Lwin_now

    bl spawn_food
    b .Ladvance_done

.Llose_now:
    adrp x9, _game_state@PAGE
    add x9, x9, _game_state@PAGEOFF
    mov x10, #1
    str x10, [x9]
    b .Ladvance_done

.Lwin_now:
    adrp x9, _game_state@PAGE
    add x9, x9, _game_state@PAGEOFF
    mov x10, #2
    str x10, [x9]

.Ladvance_done:
    ldp x27, x28, [sp, #80]
    ldp x25, x26, [sp, #64]
    ldp x23, x24, [sp, #48]
    ldp x21, x22, [sp, #32]
    ldp x19, x20, [sp, #16]
    ldp x29, x30, [sp], #96
    ret

draw_frame:
    stp x29, x30, [sp, #-80]!
    stp x19, x20, [sp, #16]
    stp x21, x22, [sp, #32]
    stp x23, x24, [sp, #48]
    stp x25, x26, [sp, #64]
    mov x29, sp

    adrp x0, _home_seq@PAGE
    add x0, x0, _home_seq@PAGEOFF
    mov x1, #3
    bl write_stdout

    adrp x0, _status_fmt@PAGE
    add x0, x0, _status_fmt@PAGEOFF
    mov x1, #20
    bl write_stdout

    adrp x9, _score_value@PAGE
    add x9, x9, _score_value@PAGEOFF
    ldr x0, [x9]
    bl write_u64_stdout

    adrp x0, _status_mid@PAGE
    add x0, x0, _status_mid@PAGEOFF
    mov x1, #11
    bl write_stdout

    adrp x9, _snake_length@PAGE
    add x9, x9, _snake_length@PAGEOFF
    ldr x0, [x9]
    bl write_u64_stdout

    adrp x0, _crlf@PAGE
    add x0, x0, _crlf@PAGEOFF
    mov x1, #2
    bl write_stdout

    adrp x0, _border_line@PAGE
    add x0, x0, _border_line@PAGEOFF
    mov x1, #42
    bl write_stdout

    adrp x0, _crlf@PAGE
    add x0, x0, _crlf@PAGEOFF
    mov x1, #2
    bl write_stdout

    mov x19, #0

.Lrow_loop:
    cmp x19, #16
    b.ge .Lrows_done

    mov x0, #124
    bl write_char_stdout

    mov x20, #0

.Lcol_loop:
    cmp x20, #40
    b.ge .Lrow_end

    mov x0, x20
    mov x1, x19
    bl cell_char
    bl write_char_stdout

    add x20, x20, #1
    b .Lcol_loop

.Lrow_end:
    mov x0, #124
    bl write_char_stdout

    adrp x0, _crlf@PAGE
    add x0, x0, _crlf@PAGEOFF
    mov x1, #2
    bl write_stdout

    add x19, x19, #1
    b .Lrow_loop

.Lrows_done:
    adrp x0, _border_line@PAGE
    add x0, x0, _border_line@PAGEOFF
    mov x1, #42
    bl write_stdout

    adrp x0, _crlf@PAGE
    add x0, x0, _crlf@PAGEOFF
    mov x1, #2
    bl write_stdout

    adrp x0, _controls_line@PAGE
    add x0, x0, _controls_line@PAGEOFF
    mov x1, #31
    bl write_stdout

    adrp x0, _crlf@PAGE
    add x0, x0, _crlf@PAGEOFF
    mov x1, #2
    bl write_stdout

    ldp x25, x26, [sp, #64]
    ldp x23, x24, [sp, #48]
    ldp x21, x22, [sp, #32]
    ldp x19, x20, [sp, #16]
    ldp x29, x30, [sp], #80
    ret

cell_char:
    adrp x9, _food_x@PAGE
    add x9, x9, _food_x@PAGEOFF
    ldr x10, [x9]
    cmp x0, x10
    b.ne .Lcheck_head

    adrp x9, _food_y@PAGE
    add x9, x9, _food_y@PAGEOFF
    ldr x10, [x9]
    cmp x1, x10
    b.eq .Lreturn_food

.Lcheck_head:
    adrp x9, _snake_x@PAGE
    add x9, x9, _snake_x@PAGEOFF
    ldr x10, [x9, #0]
    cmp x0, x10
    b.ne .Lcheck_body

    adrp x9, _snake_y@PAGE
    add x9, x9, _snake_y@PAGEOFF
    ldr x10, [x9, #0]
    cmp x1, x10
    b.eq .Lreturn_head

.Lcheck_body:
    adrp x9, _snake_length@PAGE
    add x9, x9, _snake_length@PAGEOFF
    ldr x10, [x9]
    cmp x10, #1
    b.le .Lreturn_space

    adrp x11, _snake_x@PAGE
    add x11, x11, _snake_x@PAGEOFF
    adrp x12, _snake_y@PAGE
    add x12, x12, _snake_y@PAGEOFF
    mov x13, #1

.Lbody_loop:
    cmp x13, x10
    b.ge .Lreturn_space
    ldr x14, [x11, x13, lsl #3]
    cmp x0, x14
    b.ne .Lbody_next
    ldr x14, [x12, x13, lsl #3]
    cmp x1, x14
    b.eq .Lreturn_body

.Lbody_next:
    add x13, x13, #1
    b .Lbody_loop

.Lreturn_food:
    mov x0, #42
    ret

.Lreturn_head:
    mov x0, #64
    ret

.Lreturn_body:
    mov x0, #111
    ret

.Lreturn_space:
    mov x0, #32
    ret

.data

_status_fmt:
    .asciz "Live Snake   Score: "

_status_mid:
    .asciz "   Length: "

_lose_fmt:
    .asciz "Game over. Final score: "

_win_fmt:
    .asciz "You cleared every food pickup. Final score: "

_quit_fmt:
    .asciz "Quit. Final score: "

_border_line:
    .asciz "+----------------------------------------+"

_controls_line:
    .asciz "Controls: W A S D move, q quits"

_not_tty_msg:
    .asciz "16_snake_live.s needs a real terminal (TTY)."

_setup_fail_msg:
    .asciz "Could not switch the terminal into live mode."

_clear_hide_seq:
    .byte 27
    .ascii "[2J"
    .byte 27
    .ascii "[H"
    .byte 27
    .ascii "[?25l"

_home_seq:
    .byte 27
    .ascii "[H"

_restore_seq:
    .byte 27
    .ascii "[?25h"
    .byte 27
    .ascii "[0m\n"

_crlf:
    .byte 13
    .byte 10

_food_table:
    .quad 30, 8
    .quad 30, 11
    .quad 12, 11
    .quad 12, 3
    .quad 35, 3
    .quad 35, 13
    .quad 6, 13
    .quad 6, 5

.section __DATA,__bss
.p2align 3

_saved_flags:
    .space 8

_flags_valid:
    .space 8

_terminal_active:
    .space 8

_snake_length:
    .space 8

_snake_dir_x:
    .space 8

_snake_dir_y:
    .space 8

_score_value:
    .space 8

_game_state:
    .space 8

_food_index:
    .space 8

_food_x:
    .space 8

_food_y:
    .space 8

_input_byte:
    .space 8

_original_termios:
    .space 72

_raw_termios:
    .space 72

_snake_x:
    .space 512

_snake_y:
    .space 512
