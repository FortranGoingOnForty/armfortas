! Regression: a scalar derived-type VALUE whose storage is descriptor-sized
! (its only component is an allocatable array) lowers to the same
! `Ptr<[i8;384]>` IR type as an array DESCRIPTOR. When such a value was
! passed DIRECTLY as an actual argument (a function/constructor result temp,
! not a named variable), the by-ref arg lowering wrongly treated it as an
! array descriptor and loaded its first 8 bytes as a base_addr, passing
! garbage -> the callee saw the allocatable component as unallocated.
!
! The discriminator is the actual's RANK: an array actual (rank>=1) needs
! base_addr extraction for assumed-size/explicit-shape dummies; a scalar
! (rank 0) derived value must be passed by address. Surfaced building
! toml-f: `set_value(table, toml_path("a","b"), val)` where the generic
! `toml_path` constructor returns `type(toml_path)` whose only component is
! `type(toml_key), allocatable :: path(:)` -> walk_path saw an unallocated
! path and returned a fatal stat. x12 campaign.
!
! CHECK: named size=2 k1=first
! CHECK: temp size=2 k1=first
! CHECK: generic size=2 k1=first
module x12_dctac
  implicit none
  type :: key
    character(:), allocatable :: k
    integer :: origin = 0
  end type
  type :: path
    type(key), allocatable :: p(:)
  end type
  ! generic constructor sharing the derived-type name (the toml_path shape)
  interface path
    module procedure new_path2
  end interface
contains
  pure function new_path2(k1, k2) result(self)
    character(*), intent(in) :: k1, k2
    type(path) :: self
    allocate(self%p(2))
    self%p(1)%k = k1
    self%p(2)%k = k2
  end function

  function mk(k1, k2) result(self)
    character(*), intent(in) :: k1, k2
    type(path) :: self
    self = path(k1, k2)
  end function

  subroutine use_path(tag, pp)
    character(*), intent(in) :: tag
    type(path), intent(in) :: pp
    if (allocated(pp%p)) then
      write(*, '(a," size=",i0," k1=",a)') tag, size(pp%p), pp%p(1)%k
    else
      write(*, '(a," UNALLOCATED (bug)")') tag
    end if
  end subroutine
end module

program main
  use x12_dctac
  implicit none
  type(path) :: pv

  pv = path("first", "second")
  call use_path("named", pv)                       ! named var (already worked)
  call use_path("temp", mk("first", "second"))     ! plain function-result temp
  call use_path("generic", path("first", "second"))! generic-constructor temp
end program
