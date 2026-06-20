! Regression: a USE-renamed derived type must dispatch like its original.
! jonquil does `use tomlf, only: json_value => toml_value`, so a
! `class(json_value)` actual has to match a specific declared with
! `class(toml_value)`. Generic dispatch compared the raw type names
! ("json_value" vs "toml_value") and matched neither candidate, aborting.
! It now canonicalizes a renamed derived type to its underlying name.
! Surfaced building fpm: jonquil json_load. x12.
!
! CHECK: picked file
! CHECK: picked unit
module base
  implicit none
  type :: toml_value
    integer :: x
  end type
  interface gv
    module procedure gv_file
    module procedure gv_unit
  end interface
contains
  subroutine gv_file(o, name)
    class(toml_value), allocatable, intent(out) :: o
    character(*), intent(in) :: name
    allocate(o)
    write(*, '(a)') 'picked file'
  end subroutine
  subroutine gv_unit(o, io)
    class(toml_value), allocatable, intent(out) :: o
    integer, intent(in) :: io
    allocate(o)
    write(*, '(a)') 'picked unit'
  end subroutine
end module
module wrap
  use base, only: gv, json_value => toml_value
  implicit none
contains
  subroutine doit()
    class(json_value), allocatable :: jv
    call gv(jv, 'hello')
    call gv(jv, 7)
  end subroutine
end module
program p
  use wrap
  call doit()
end program p
