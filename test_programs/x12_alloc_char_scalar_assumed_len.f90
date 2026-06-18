! Regression: an explicit-length allocatable character scalar
! (`character(len=N), allocatable`) passed to a `character(len=*)` dummy
! must pass length N, not 0. armfortas computed the actual-argument length
! via local_char_runtime_len, whose CharKind::None arm returned None for a
! fixed-length allocatable char scalar (it is deliberately NOT
! descriptor-backed), so the callee received length 0 and saw an empty
! string. Surfaced building fortsh: readline's `current_input`
! (character(len=MAX_LINE_LEN), allocatable) is passed to
! compute_history_suggestion's len=* dummy, so the prefix read empty and
! autosuggestion never matched any history entry. x12.
!
! CHECK: caller len=64
! CHECK: callee len=64
! CHECK: callee s=[hello]
module m
contains
  subroutine show(s)
    character(len=*), intent(in) :: s
    write(*, '(A,I0)') 'callee len=', len(s)
    write(*, '(A,A,A)') 'callee s=[', trim(s), ']'
  end subroutine
end module
program p
  use m
  character(len=64), allocatable :: a
  allocate(a)
  a = 'hello'
  write(*, '(A,I0)') 'caller len=', len(a)
  call show(a)
end program p
