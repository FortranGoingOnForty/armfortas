! Regression (fpm cmd_new SIGSEGV writing a verified fpm.toml): a
! type-bound-procedure vtable dispatch of a SUBROUTINE `get` must not
! acquire a hidden result argument from a same-named module FUNCTION.
!
! fpm defines both tomlf's `subroutine get(self, key, ptr)` (a deferred TBP)
! and M_CLI2's `function get(key) result(character(:) allocatable)`. The
! indirect (vtable) dispatch resolved the callee's hidden-result ABI by the
! bare name "get" via find_linkable_symbol_any_scope, which returns the first
! callable in source order. With the function defined first it picked up the
! function's allocatable-string result and prepended a 32-byte hidden-result
! descriptor to the subroutine dispatch. That shifted every argument by one --
! self became a bogus descriptor -- and the call dereferenced garbage (rc=139).
! The fix: a subroutine CALL dispatch (explicit return type Void) never
! synthesizes a hidden result. The competing function is defined FIRST here so
! the bare-name lookup resolves to it -- reorder and the bug hides.
module m_cli
  implicit none
  private
  public :: get
contains
  function get(key) result(valout)
     character(len=*), intent(in) :: key
     character(len=:), allocatable :: valout
     valout = 'val:'//key
  end function get
end module

module m_tbl
  implicit none
  private
  public :: base_t, ext_t, run_dispatch
  type, abstract :: base_t
   contains
     procedure(si), deferred :: get
  end type
  abstract interface
     subroutine si(self, key, x)
       import base_t
       class(base_t), intent(inout) :: self
       character(len=*), intent(in) :: key
       integer, intent(out) :: x
     end subroutine
  end interface
  type, extends(base_t) :: ext_t
     integer :: n = 0
   contains
     procedure :: get
  end type
contains
  subroutine get(self, key, x)
     class(ext_t), intent(inout) :: self
     character(len=*), intent(in) :: key
     integer, intent(out) :: x
     x = self%n * 2 + len(key)
  end subroutine
  subroutine run_dispatch(obj)
     class(base_t), intent(inout) :: obj   ! polymorphic -> genuine vtable dispatch
     integer :: v
     call obj%get('ab', v)
     write(*, '(a,i0)') 'v=', v
     if (v /= 86) error stop 1
  end subroutine
end module

program p
  use m_tbl, only: ext_t, run_dispatch
  implicit none
  type(ext_t) :: e
  e%n = 42
  call run_dispatch(e)
  write(*, '(a)') 'DONE'
end program p
! CHECK: v=86
! CHECK: DONE
