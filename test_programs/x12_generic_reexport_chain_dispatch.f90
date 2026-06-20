! Regression: a generic call from inside a module that extends a generic
! it imports must see the full re-exported specific set. fpm_toml does
! `use tomlf, only: get_value` plus a local `interface get_value`, and
! tomlf re-exports get_value from several toml-f submodules through a
! chain. The merged interface symbol records only its local specifics
! plus the first re-exported one, so an in-module call resolved against
! an incomplete candidate list and aborted. resolve_subroutine_call_name
! now gathers the full candidate set (including all use-associated
! specifics), not just the symbol's own arg_names. x12.
!
! CHECK: t1=11
! CHECK: t2=21
! CHECK: t4=41
! CHECK: t5=51
module la
  type :: t1; integer :: x; end type
  interface gv; module procedure a1; end interface
contains
  subroutine a1(s, r); type(t1), intent(in) :: s; integer, intent(out) :: r; r = 11; end subroutine
end module
module lk
  type :: t2; integer :: x; end type
  interface gv; module procedure k1; end interface
contains
  subroutine k1(s, r); type(t2), intent(in) :: s; integer, intent(out) :: r; r = 21; end subroutine
end module
module lt
  type :: t4; integer :: x; end type
  interface gv; module procedure tt1; end interface
contains
  subroutine tt1(s, r); type(t4), intent(in) :: s; integer, intent(out) :: r; r = 41; end subroutine
end module
module build
  use la, only: gv, t1
  use lk, only: gv, t2
  use lt, only: gv, t4
  public
end module
module umb
  use build, only: gv, t1, t2, t4
  public
end module
module fpmlike
  use umb, only: gv, t1, t2, t4
  implicit none
  type :: t5; integer :: x; end type
  interface gv
    module procedure f5
  end interface gv
  public
contains
  subroutine f5(s, r); type(t5), intent(in) :: s; integer, intent(out) :: r; r = 51; end subroutine
  subroutine driver()
    type(t1) :: a1o
    type(t2) :: a2o
    type(t4) :: a4o
    type(t5) :: a5o
    integer :: r
    call gv(a1o, r); write(*, '(a,i0)') 't1=', r
    call gv(a2o, r); write(*, '(a,i0)') 't2=', r
    call gv(a4o, r); write(*, '(a,i0)') 't4=', r
    call gv(a5o, r); write(*, '(a,i0)') 't5=', r
  end subroutine
end module
program p
  use fpmlike
  call driver()
end program p
