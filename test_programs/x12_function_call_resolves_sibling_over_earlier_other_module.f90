! Regression (fpm build ERROR STOP parsing fpm.toml): a bare FUNCTION call from
! a module procedure must bind to the sibling the caller can actually see, not
! to the first same-named function found in source order across all modules.
!
! The function-call lowering fell back to a global scan (find_linkable_symbol_
! any_scope) that returns the first callable in source order. tomlf's lexer
! `next_token` calls its sibling `match(lexer,pos,kind)`, but fpm_versioning's
! earlier `match(lhs,rhs)` won the scan, so TOML lexing jumped into version
! matching and hit an error stop. The fix resolves through the caller's scope
! first (current_proc_scope), matching the subroutine-call path.
module m_ver
  implicit none
  private
  public :: match
contains
  ! defined FIRST; unrelated 2-arg version match. If mis-called it error stops.
  logical function match(lhs, rhs)
     integer, intent(in) :: lhs, rhs
     match = (lhs == rhs)
     error stop 'WRONG-match'
  end function match
end module

module m_lex
  implicit none
  private
  public :: countx
contains
  ! sibling helper 'match' with a DIFFERENT signature (3 args).
  logical function match(buf, pos, kind)
     character(len=*), intent(in) :: buf
     integer, intent(in) :: pos
     character(len=1), intent(in) :: kind
     match = (buf(pos:pos) == kind)
  end function match
  integer function countx(buf) result(n)
     character(len=*), intent(in) :: buf
     integer :: i
     n = 0
     do i = 1, len(buf)
        if (match(buf, i, 'x')) n = n + 1   ! must bind m_lex%match, not m_ver%match
     end do
  end function countx
end module

program p
  use m_lex, only : countx
  implicit none
  integer :: n
  n = countx('axbxxc')
  write(*,'(a,i0)') 'N=', n
  print '(a)', 'DONE'
end program p
! CHECK: N=3
! CHECK: DONE
