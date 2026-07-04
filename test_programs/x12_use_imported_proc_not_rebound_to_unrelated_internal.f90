! Regression (fpm ERROR STOP parsing toml-f's manifest): a use-imported module
! procedure whose bare name matches an INTERNAL subprogram of an unrelated
! procedure must not be rebound to that internal subprogram.
!
! same_unit_func_ref rebinds already-resolved callees to contained procedures
! so host-associated internal calls link to their mangled internal names. Its
! caller-relative walk fell back to a GLOBAL last-match scan, so tomlf's
! ordered-map push_back — whose generic `resize` correctly resolved to
! tomlf_structure_node's specific (bare name "resize") — was rebound to
! tomlf_de_parser::parse_table_header's internal toml_key `resize`. The
! toml_key copy walked the node array with the wrong stride and scribbled
! text pointers into polymorphic val slots; the next dispatch hit the
! vtable-fail ERROR STOP. The fix makes the rebind host-association-only.
!
! Here: m_grow's procedure has an internal `resize` for key_t (40-byte
! entries, allocatable char); m_node exports a module procedure literally
! named `resize` for node_t (8-byte entries). m_map imports m_node's and
! grows a node list past its initial size. Without the fix the call binds
! the key_t resize and the node data is destroyed.

module m_node
  implicit none
  private
  public :: node_t, resize
  type :: node_t
     integer :: a = 0
     integer :: b = 0
  end type
contains
  subroutine resize(lst)   ! module procedure literally named `resize`
     type(node_t), allocatable, intent(inout) :: lst(:)
     type(node_t), allocatable :: tmp(:)
     integer :: m, j
     if (allocated(lst)) then
        m = 2 * size(lst)
        call move_alloc(lst, tmp)
        allocate(lst(m))
        do j = 1, size(tmp); lst(j) = tmp(j); end do
     else
        allocate(lst(4))
     end if
  end subroutine
end module

module m_grow
  implicit none
  private
  public :: key_t, build_stack
  type :: key_t
     character(len=:), allocatable :: k
     integer :: origin = 0
  end type
contains
  subroutine build_stack(n, total)
     integer, intent(in) :: n
     integer, intent(out) :: total
     type(key_t), allocatable :: stack(:)
     integer :: i
     do i = 1, n
        if (.not.allocated(stack)) call resize(stack)
        if (i > size(stack)) call resize(stack)
        stack(i)%k = 'k'
        stack(i)%origin = i
     end do
     total = size(stack)
  contains
     subroutine resize(s)   ! internal subprogram named `resize`, key_t
        type(key_t), allocatable, intent(inout) :: s(:)
        type(key_t), allocatable :: tmp(:)
        integer :: m, j
        if (allocated(s)) then
           m = 2 * size(s)
           call move_alloc(s, tmp)
           allocate(s(m))
           do j = 1, size(tmp); s(j) = tmp(j); end do
        else
           allocate(s(4))
        end if
     end subroutine
  end subroutine
end module

module m_map
  use m_node, only : node_t, resize
  implicit none
  private
  public :: fill
contains
  subroutine fill(n, checksum)
     integer, intent(in) :: n
     integer, intent(out) :: checksum
     type(node_t), allocatable :: lst(:)
     integer :: i
     do i = 1, n
        if (.not.allocated(lst)) call resize(lst)
        if (i > size(lst)) call resize(lst)
        lst(i)%a = i
        lst(i)%b = 2*i
     end do
     checksum = 0
     do i = 1, n
        checksum = checksum + lst(i)%a + lst(i)%b
     end do
  end subroutine
end module

program p
  use m_grow, only : build_stack
  use m_map, only : fill
  implicit none
  integer :: total, checksum
  call build_stack(9, total)
  write(*,'(a,i0)') 'stack=', total
  call fill(20, checksum)
  ! sum(i + 2i, i=1..20) = 3 * 20*21/2 = 630
  write(*,'(a,i0)') 'checksum=', checksum
  print '(a)', 'DONE'
end program
! CHECK: stack=16
! CHECK: checksum=630
! CHECK: DONE
