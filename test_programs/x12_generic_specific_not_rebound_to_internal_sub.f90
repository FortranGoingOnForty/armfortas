! Regression (fpm manifest diagnostics crashed instead of rendering): a
! char-returning call resolved through a GENERIC must link the resolved
! specific, not be rebound by bare name to an internal subprogram of an
! unrelated procedure (nor to a same-named local module procedure).
!
! tomlf_diagnostic's render calls the generic integer->string `to_string`;
! the resolver picked tomlf_utils' to_string_i4 and built its 2-arg call
! vector (hidden descriptor + val), but same_unit_func_ref matched the bare
! "to_string" against the unit-wide internal-subprogram map and rewrote the
! callee to the caller module's own 3-param to_string(val, width) — 2 args
! welded onto a 3-param function, so `width` read garbage and every fpm
! manifest diagnostic SIGSEGV'd. Two fixes: post-generic-resolution the
! bare generic name is excluded from the rebind keys, and the rebind walk
! is host-association-only.
!
! Here: m_other's procedure has an internal function named `fmt`; m_util
! exports a generic `fmt` whose specific is also named `fmt` (1-arg,
! char-returning). m_render calls the generic with one arg while defining
! its OWN 2-arg module function also named... kept distinct: the hijack
! shape needs only the internal-name collision on the specific's bare name.

module m_util
  implicit none
  private
  public :: fmt
  interface fmt
     module procedure fmt
  end interface
contains
  pure function fmt(val) result(s)
     integer, intent(in) :: val
     character(len=:), allocatable :: s
     character(len=16) :: buf
     write(buf,'(i0)') val
     s = trim(buf)
  end function
end module

module m_other
  implicit none
  private
  public :: unrelated
contains
  subroutine unrelated(x, out)
     integer, intent(in) :: x
     character(len=:), allocatable, intent(out) :: out
     out = helper(x)
  contains
     pure function fmt(a, b) result(s)   ! internal `fmt`, DIFFERENT signature
        integer, intent(in) :: a, b
        character(len=:), allocatable :: s
        character(len=16) :: buf
        write(buf,'(i0)') a*1000 + b
        s = trim(buf)
     end function
     function helper(a) result(s)
        integer, intent(in) :: a
        character(len=:), allocatable :: s
        s = fmt(a, 7)
     end function
  end subroutine
end module

module m_render
  use m_util, only : fmt
  implicit none
  private
  public :: render
contains
  function render(line) result(s)
     integer, intent(in) :: line
     character(len=:), allocatable :: s
     s = 'line '//fmt(line)//';'
  end function
end module

program p
  use m_render, only : render
  use m_other, only : unrelated
  implicit none
  character(len=:), allocatable :: t
  call unrelated(4, t)
  write(*,'(a)') 'internal='//t
  write(*,'(a)') 'render='//render(42)
  print '(a)', 'DONE'
end program
! CHECK: internal=4007
! CHECK: render=line 42;
! CHECK: DONE
