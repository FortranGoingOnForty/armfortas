! CHECK: base_to_ext 22
! CHECK: ext_to_base 11
! CHECK: same_ext 33
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar2_poly_realloc
  implicit none

  type :: base
    integer :: b = 1
  end type base

  type, extends(base) :: ext
    integer :: e = 7
  end type ext

  class(base), allocatable :: p
  class(base), allocatable :: q

  allocate(ext :: p)
  select type (p)
  type is (ext)
    p%b = 10
    p%e = 22
  end select

  allocate(base :: q)
  q = p
  select type (q)
  type is (ext)
    print '(a,1x,i0)', 'base_to_ext', q%e
  type is (base)
    print '(a)', 'base_to_ext STILL_BASE'
  class default
    print '(a)', 'base_to_ext DEFAULT'
  end select

  deallocate(p)
  allocate(base :: p)
  select type (p)
  type is (base)
    p%b = 11
  end select

  q = p
  select type (q)
  type is (base)
    print '(a,1x,i0)', 'ext_to_base', q%b
  type is (ext)
    print '(a)', 'ext_to_base STILL_EXT'
  class default
    print '(a)', 'ext_to_base DEFAULT'
  end select

  deallocate(p)
  allocate(ext :: p)
  select type (p)
  type is (ext)
    p%e = 33
  end select

  q = p
  select type (q)
  type is (ext)
    print '(a,1x,i0)', 'same_ext', q%e
  type is (base)
    print '(a)', 'same_ext BASE'
  class default
    print '(a)', 'same_ext DEFAULT'
  end select
end program ar2_poly_realloc
