! Address-taken internal procedures called through procedure dummies keep
! their full ABI even when an apparently unused leading argument exists.
!
! CHECK: 99
! CHECK: he_alpha
! CHECK: he_beta
module ar13_deadarg_m
  implicit none

  type :: span_t
    character(len=:), allocatable :: prefix
  end type span_t

  type :: item_t
    character(len=:), allocatable :: text
  end type item_t

  abstract interface
    subroutine int_provider(unused, val, out)
      integer, intent(in) :: unused
      integer, intent(in) :: val
      integer, intent(out) :: out
    end subroutine int_provider

    subroutine string_provider(unused, span, out)
      import :: span_t
      integer, intent(in) :: unused
      type(span_t), intent(in) :: span
      character(len=:), allocatable, intent(out) :: out
    end subroutine string_provider

    subroutine items_provider(unused, span, items)
      import :: item_t, span_t
      integer, intent(in) :: unused
      type(span_t), intent(in) :: span
      type(item_t), allocatable, intent(out) :: items(:)
    end subroutine items_provider
  end interface

contains
  subroutine refresh_int(p)
    procedure(int_provider) :: p
    integer :: out
    call p(0, 99, out)
    print '(i0)', out
  end subroutine refresh_int

  subroutine refresh_string(p)
    procedure(string_provider) :: p
    type(span_t) :: span
    character(len=:), allocatable :: out
    span%prefix = 'he'
    call p(0, span, out)
    print '(a)', out
  end subroutine refresh_string

  subroutine refresh_items(p)
    procedure(items_provider) :: p
    type(span_t) :: span
    type(item_t), allocatable :: items(:)
    span%prefix = 'he'
    call p(0, span, items)
    print '(a)', items(1)%text
  end subroutine refresh_items
end module ar13_deadarg_m

program ar13_deadarg_indirect
  use ar13_deadarg_m
  implicit none
  call refresh_int(prov_int)
  call refresh_string(prov_string)
  call refresh_items(prov_items)

contains
  subroutine prov_int(unused, val, out)
    integer, intent(in) :: unused
    integer, intent(in) :: val
    integer, intent(out) :: out
    out = val
  end subroutine prov_int

  subroutine prov_string(unused, span, out)
    integer, intent(in) :: unused
    type(span_t), intent(in) :: span
    character(len=:), allocatable, intent(out) :: out
    out = span%prefix // '_alpha'
  end subroutine prov_string

  subroutine prov_items(unused, span, items)
    integer, intent(in) :: unused
    type(span_t), intent(in) :: span
    type(item_t), allocatable, intent(out) :: items(:)
    allocate(items(1))
    items(1)%text = span%prefix // '_beta'
  end subroutine prov_items
end program ar13_deadarg_indirect
